//! The model's edit tool: apply one unified diff to one file under the checkout.
//!
//! The editor is the runtime's [`mokmok_agent_runtime::Editor`] port for the kernel. It parses the
//! patch, asks the authority, reads the file, stores the pre-image and post-image, and renders
//! the observation. `commit` performs the atomic rename through a directory fd opened with
//! `RESOLVE_NO_SYMLINKS`, so a symlink in any path component is refused, never followed.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mokmok_agent_runtime::{
    ArtifactRef, EditCall, EditOutcome, EditReport, Editor as EditorPort, Plan, Ready, edit,
};
use mokmok_artifact::{BlobStore, ContentHash};
use mokmok_patch::{ApplyError, Label, ParseError, Patch, Placed};
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::Mode;

use crate::authority::Authority;
use crate::clock::Clock;
use crate::config::{Config, CrashPoint};
use crate::error::Error;

/// A target file may not be larger than this; a larger file is refused rather than truncated.
const MAX_EDIT_FILE_BYTES: usize = 4 * 1024 * 1024;

/// The adapter that turns one patch into one atomic file write.
pub struct Editor {
    config: Config,
    /// The resolved checkout path, which every open, read, and write goes through, so no symlink is
    /// followed during I/O.
    checkout: PathBuf,
    /// The checkout path as the operator wrote it. `Authority` compares this against the literal
    /// `--allow-write` roots, so a checkout the operator reached through a symlink is still covered
    /// by the grant that names that symlink.
    checkout_literal: PathBuf,
    store: BlobStore,
    clock: Arc<dyn Clock>,
}

impl Editor {
    /// Opens the editor for one checkout and clock.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Artifact`] when the blob store cannot be opened, and [`Error::Io`] when
    /// the checkout cannot be resolved.
    pub fn new(
        config: &Config,
        checkout: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        Ok(Self {
            config: config.clone(),
            checkout: mokmok_sandbox::resolve(checkout)?,
            checkout_literal: checkout.to_path_buf(),
            store: BlobStore::open(&config.artifacts)?,
            clock,
        })
    }

    /// Renders a patch-crate error as the sentence the model reads.
    fn patch_reason(error: PatchError) -> String {
        match error {
            PatchError::Parse(ParseError::NoFileSection) => "no file section opened".to_owned(),
            PatchError::Parse(ParseError::NoHunks) => "the file section had no hunks".to_owned(),
            PatchError::Parse(ParseError::InvalidHunkLine(line)) => {
                format!("invalid hunk line: {line}")
            },
            PatchError::Parse(ParseError::InvalidHeader(header)) => {
                format!("invalid hunk header: {header}")
            },
            PatchError::Parse(ParseError::InvalidPath(path)) => {
                format!(
                    "invalid path: {path} — name the file relative to the checkout, as its --- \
                     and +++ labels"
                )
            },
            PatchError::Apply(ApplyError::NotText) => {
                "the file is not valid UTF-8, so no hunk can be matched against it".to_owned()
            },
            PatchError::Apply(ApplyError::NothingToApply) => {
                "the source is absent, so a hunk with an old side has nothing to match".to_owned()
            },
            PatchError::Apply(ApplyError::ZeroStart { hunk }) => {
                format!(
                    "hunk {} starts at 0 in a file that exists",
                    hunk.saturating_add(1)
                )
            },
            PatchError::Apply(ApplyError::NotFound { hunk }) => {
                format!(
                    "hunk {} does not match the file from its stated line forward",
                    hunk.saturating_add(1)
                )
            },
            PatchError::TooManyFiles => {
                "the patch must name exactly one file, and it named more than one".to_owned()
            },
            PatchError::NoFile => "the patch named no file section".to_owned(),
        }
    }

    /// Builds the report a commit-time refusal records.
    fn refused_report(
        relative: &str,
        reason: String,
    ) -> EditReport {
        let outcome = EditOutcome::Refused { reason };
        let observation = edit::observation(relative, &dummy_ref(), &dummy_ref(), &outcome, "");

        EditReport {
            outcome,
            observation,
        }
    }
}

impl EditorPort for Editor {
    // The plan is a straight-line pipeline: parse, resolve, gate, read, apply, store, check.
    // Splitting it would move error conversions into helpers that return the runtime's `Plan`,
    // which is less readable than the sequential checks.
    #[allow(clippy::too_many_lines)]
    fn plan(
        &self,
        call: &EditCall,
    ) -> Result<Plan, mokmok_agent_runtime::Error> {
        let at = self.clock.now();
        let authority = Authority::new(&self.config, at)?;
        let parsed = match Patch::parse(&call.patch) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(Plan::Refused(Self::patch_reason(PatchError::Parse(error))));
            },
        };

        if parsed.files.is_empty() {
            return Ok(Plan::Refused(Self::patch_reason(PatchError::NoFile)));
        }
        if parsed.files.len() > 1 {
            return Ok(Plan::Refused(Self::patch_reason(PatchError::TooManyFiles)));
        }

        let section = &parsed.files[0];
        let relative = match &section.target {
            Label::Absent => {
                return Ok(Plan::Refused(
                    "this tool rewrites one file; to delete one, run a command".to_owned(),
                ));
            },
            Label::Path(path) => path.clone(),
        };

        let target = self.checkout.join(&relative);
        if !target.starts_with(&self.checkout) {
            return Ok(Plan::Refused(format!(
                "{relative} leaves the checkout, and the patch labels must name a file inside it"
            )));
        }

        let Some(parent) = target.parent() else {
            return Ok(Plan::Refused(format!(
                "{relative} has no parent directory; create it with a command"
            )));
        };
        let Some(name) = target.file_name() else {
            return Ok(Plan::Refused(
                "the path names no file; name the file relative to the checkout".to_owned(),
            ));
        };

        let parent_fd = match mokmok_sandbox::open_resolved_dir(parent) {
            Ok(fd) => fd,
            Err(error) => {
                return Ok(Plan::Refused(open_reason(
                    &error,
                    &relative,
                    parent,
                    &format!("{relative} could not be opened"),
                )));
            },
        };

        // The authority answers for the path the operator wrote, because `--allow-write` is
        // compared literally; every open below uses the resolved one.
        if let Err(denial) = authority.may_write_at(&self.checkout_literal.join(&relative), at) {
            return Ok(Plan::Refused(format!(
                "{relative} is not under a directory the operator granted for writing, so nothing \
                 was written: {denial}"
            )));
        }

        let (before_bytes, _mode, creates) = match read_target(&parent_fd, name, &relative) {
            Ok(Some(value)) => {
                if matches!(section.source, Label::Absent) {
                    return Ok(Plan::Refused(format!(
                        "{relative} already exists; patch it instead of creating it"
                    )));
                }
                (value.0, value.1, false)
            },
            Ok(None) => {
                if !matches!(section.source, Label::Absent) {
                    return Ok(Plan::Refused(format!(
                        "{relative} does not exist; create it with a command before patching it"
                    )));
                }
                (Vec::new(), DEFAULT_MODE, true)
            },
            Err(reason) => return Ok(Plan::Refused(reason)),
        };

        if before_bytes.len() > MAX_EDIT_FILE_BYTES {
            return Ok(Plan::Refused(format!(
                "{relative} is {} bytes, and this tool refuses files larger than {} bytes",
                before_bytes.len(),
                MAX_EDIT_FILE_BYTES,
            )));
        }

        let applied = match section.apply(&before_bytes) {
            Ok(applied) => applied,
            Err(error) => {
                return Ok(Plan::Refused(format!(
                    "{relative}: {}",
                    Self::patch_reason(PatchError::Apply(error))
                )));
            },
        };

        let before_hash = self.store.put(&before_bytes).map_err(Error::Artifact)?;
        let after_hash = self.store.put(&applied.bytes).map_err(Error::Artifact)?;

        let before = artifact_ref(
            &before_hash,
            u64::try_from(before_bytes.len()).unwrap_or(u64::MAX),
        );
        let after = artifact_ref(
            &after_hash,
            u64::try_from(applied.bytes.len()).unwrap_or(u64::MAX),
        );

        if let Some(expected) = &call.expect {
            if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(Plan::Refused(format!(
                    "{relative}: expect is not a 64-character hexadecimal sha256 hash"
                )));
            }
            let actual = before_hash.to_string();
            if !actual.eq_ignore_ascii_case(expected) {
                // The call carries the expected hash and no expected size, so the refusal may not
                // name one; the size it prints is the file's own.
                return Ok(Plan::Refused(format!(
                    "{relative} is {} bytes (sha256 {actual}), not the file (sha256 {expected}) the \
                     call expected",
                    before.bytes,
                )));
            }
        }

        let placement = render_placement(&applied.hunks);

        Ok(Plan::Ready(Ready {
            relative,
            before,
            after,
            bytes: applied.bytes,
            placement,
            creates,
        }))
    }

    fn commit(
        &self,
        ready: &Ready,
    ) -> Result<EditReport, mokmok_agent_runtime::Error> {
        crate::config::crash(&self.config, CrashPoint::BeforeDispatch);

        let target = self.checkout.join(&ready.relative);
        let Some(parent) = target.parent() else {
            return Ok(Self::refused_report(
                &ready.relative,
                "the path has no parent directory; create it with a command".to_owned(),
            ));
        };
        let Some(name) = target.file_name() else {
            return Ok(Self::refused_report(
                &ready.relative,
                "the path names no file; name the file relative to the checkout".to_owned(),
            ));
        };

        let parent_fd = match mokmok_sandbox::open_resolved_dir(parent) {
            Ok(fd) => fd,
            Err(error) => {
                return Ok(Self::refused_report(
                    &ready.relative,
                    open_reason(
                        &error,
                        &ready.relative,
                        parent,
                        "the parent directory could not be opened",
                    ),
                ));
            },
        };

        let (current_bytes, mode) = match read_target(&parent_fd, name, &ready.relative) {
            Ok(Some(value)) => value,
            // The plan read no file because the patch creates it, so there is no pre-image to
            // compare against and the staging file is what lands.
            Ok(None) if ready.creates => (Vec::new(), DEFAULT_MODE),
            Ok(None) => {
                return Ok(Self::refused_report(
                    &ready.relative,
                    "the file disappeared before the edit could be written".to_owned(),
                ));
            },
            Err(reason) => return Ok(Self::refused_report(&ready.relative, reason)),
        };

        let current_hash = ContentHash::of(&current_bytes);
        if current_hash.to_string() != ready.before.hash {
            let found = artifact_ref(
                &current_hash,
                u64::try_from(current_bytes.len()).unwrap_or(u64::MAX),
            );
            let outcome = EditOutcome::Stale { found };
            let observation =
                edit::observation(&ready.relative, &ready.before, &ready.after, &outcome, "");
            return Ok(EditReport {
                outcome,
                observation,
            });
        }

        let tmp_name = format!(".mokmok-edit-{}", uuid::Uuid::now_v7());
        match write_atomic(
            &parent_fd,
            name,
            &tmp_name,
            &ready.bytes,
            mode,
            ready.creates,
        ) {
            Ok(()) => {
                crate::config::crash(&self.config, CrashPoint::AfterDispatch);
                Ok(EditReport {
                    outcome: EditOutcome::Applied,
                    observation: edit::observation(
                        &ready.relative,
                        &ready.before,
                        &ready.after,
                        &EditOutcome::Applied,
                        &ready.placement,
                    ),
                })
            },
            Err(WriteFailure::NothingWritten(error)) => Ok(Self::refused_report(
                &ready.relative,
                // The staging file is written before the bytes are put in place, and a create that
                // finds the name taken refuses, so the claim a refusal may make is about what this
                // edit wrote and not about what the file holds.
                format!(
                    "the write did not land ({error}), so nothing was written to {}",
                    ready.relative
                ),
            )),
            // The bytes are in place, so a refusal here would be a claim the log cannot keep.
            // Leaving the outcome unwritten hands the turn to recovery, which settles it
            // `Unknown` and renders the two images the intent recorded.
            Err(WriteFailure::LandedUnsynced(error)) => Err(Error::Io(error).into()),
        }
    }
}

/// A parse or apply error, grouped so the refusal renderer can name it.
#[derive(Debug)]
enum PatchError {
    Parse(ParseError),
    Apply(ApplyError),
    TooManyFiles,
    NoFile,
}

/// Default mode for a file the patch creates.
const DEFAULT_MODE: u32 = 0o644;

const fn dummy_ref() -> ArtifactRef {
    ArtifactRef {
        hash: String::new(),
        bytes: 0,
        truncated: false,
    }
}

fn artifact_ref(
    hash: &ContentHash,
    bytes: u64,
) -> ArtifactRef {
    ArtifactRef {
        hash: hash.to_string(),
        bytes,
        truncated: false,
    }
}

fn render_placement(hunks: &[Placed]) -> String {
    if hunks.is_empty() {
        return String::new();
    }

    hunks
        .iter()
        .enumerate()
        .map(|(index, placed)| {
            let number = index.saturating_add(1);
            match placed {
                Placed::Matched { line, offset } => {
                    format!("hunk {number} matched at line {line} (offset {offset})")
                },
                Placed::Inserted { after } => {
                    format!("hunk {number} inserted after line {after}")
                },
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_reason(
    error: &io::Error,
    relative: &str,
    path: &Path,
    fallback: &str,
) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!(
            "{} does not exist; create the parent directory with a command",
            path.display()
        )
    } else if error.raw_os_error() == Some(nix::errno::Errno::ELOOP as i32) {
        // This open is of the parent, so the link is on the way to the file rather than the file.
        format!(
            "a directory on the way to {relative} is a symbolic link, and this tool never follows \
             one"
        )
    } else {
        format!("{fallback}: {error}")
    }
}

fn read_target(
    parent_fd: &OwnedFd,
    name: &OsStr,
    relative: &str,
) -> Result<Option<(Vec<u8>, u32)>, String> {
    let file = match openat(
        parent_fd,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(nix::errno::Errno::ELOOP) => {
            return Err(format!(
                "{relative} is a symbolic link, and this tool never follows one"
            ));
        },
        Err(error) => {
            return Err(format!("{relative} could not be read: {error}"));
        },
    };

    let mut file = File::from(file);
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return Err(format!("{relative} could not be read: {error}")),
    };
    let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if size > MAX_EDIT_FILE_BYTES {
        return Err(format!(
            "{relative} is {size} bytes, and this tool refuses files larger than \
             {MAX_EDIT_FILE_BYTES} bytes"
        ));
    }

    let mut bytes = Vec::with_capacity(size);
    if let Err(error) = file.read_to_end(&mut bytes) {
        return Err(format!("{relative} could not be read: {error}"));
    }
    let mode = metadata.permissions().mode();
    Ok(Some((bytes, mode)))
}

/// Where a write stopped, because the bytes and the directory entry do not land together.
#[derive(Debug)]
enum WriteFailure {
    /// No byte of the target changed, because the rename never happened.
    NothingWritten(io::Error),
    /// `renameat` installed the bytes and the directory's own sync failed, so the file holds the
    /// post-image while the edit's survival across a crash is unproven.
    LandedUnsynced(io::Error),
}

fn write_atomic(
    parent_fd: &OwnedFd,
    name: &OsStr,
    tmp_name: &str,
    bytes: &[u8],
    mode: u32,
    creates: bool,
) -> Result<(), WriteFailure> {
    let mode = Mode::from_bits_truncate(mode & 0o777);
    let tmp = match openat(
        parent_fd,
        tmp_name,
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_NOFOLLOW,
        mode,
    ) {
        Ok(fd) => fd,
        Err(error) => return Err(WriteFailure::NothingWritten(io::Error::from(error))),
    };

    let mut tmp = File::from(tmp);
    if let Err(error) = tmp.write_all(bytes) {
        let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);
        return Err(WriteFailure::NothingWritten(error));
    }
    if let Err(error) = tmp.sync_all() {
        let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);
        return Err(WriteFailure::NothingWritten(error));
    }
    drop(tmp);

    // A patch that creates a file may take the target's name only while the target is still absent,
    // or a file that appeared meanwhile would be replaced rather than refused. A patch of an
    // existing file means to replace it, so that path renames.
    let landed = if creates {
        nix::unistd::linkat(parent_fd, tmp_name, parent_fd, name, AtFlags::empty()).map_err(
            |error| match error {
                nix::errno::Errno::EEXIST => io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the file appeared while the edit was being written",
                ),
                other => io::Error::from(other),
            },
        )
    } else {
        nix::fcntl::renameat(parent_fd, tmp_name, parent_fd, name).map_err(io::Error::from)
    };

    if let Err(error) = landed {
        let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);
        return Err(WriteFailure::NothingWritten(error));
    }
    let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);

    // Past the link or the rename the target holds the post-image, so what fails here is the
    // durability of the directory entry and not the write.
    nix::unistd::fsync(parent_fd)
        .map_err(|error| WriteFailure::LandedUnsynced(io::Error::from(error)))
}

impl From<Error> for mokmok_agent_runtime::Error {
    fn from(error: Error) -> Self {
        Self::Editor(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mokmok_agent_runtime::EditOutcome;

    #[test]
    fn the_edit_size_ceiling_is_four_mebibytes() {
        assert_eq!(MAX_EDIT_FILE_BYTES, 4_194_304);
    }

    #[test]
    fn placement_renders_matched_and_inserted_hunks() {
        let text = render_placement(&[
            Placed::Matched {
                line: 12,
                offset: 0,
            },
            Placed::Inserted { after: 5 },
        ]);

        assert!(
            text.contains("hunk 1 matched at line 12 (offset 0)"),
            "{text}"
        );
        assert!(text.contains("hunk 2 inserted after line 5"), "{text}");
    }

    #[test]
    fn refused_report_formats_the_commit_time_refusal() {
        let report = Editor::refused_report("src/x.rs", "no hunk matched".to_owned());

        assert!(
            report
                .observation
                .contains("the edit of src/x.rs was refused: no hunk matched"),
            "{}",
            report.observation
        );
        assert!(matches!(report.outcome, EditOutcome::Refused { .. }));
    }

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use mokmok_protocol::Moment;

    use super::Config;
    use crate::FixedClock;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-editor-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(
        root: &Path,
        writable: &[PathBuf],
        readable: &[PathBuf],
    ) -> crate::config::Config {
        let data = root.join("data");
        let artifacts = data.join("artifacts");
        let sandbox_root = data.join("sandbox");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&sandbox_root).unwrap();

        Config {
            data_dir: data.clone(),
            events: data.join("events.sqlite"),
            artifacts,
            sandbox_root,
            allow_tool: vec![PathBuf::from("/bin/sh")],
            allow_path: readable.to_vec(),
            allow_write: writable.to_vec(),
            search_root: Vec::new(),
            search_index: data.join("search"),
            skill_root: Vec::new(),
            tool_env: Vec::new(),
            confiner: PathBuf::from("/bin/sh"),
            crash_after: None,
            limits: mokmok_sandbox::Limits::default(),
            provider: crate::config::ProviderSettings::default(),
        }
    }

    fn edit_call(patch: &str) -> EditCall {
        EditCall {
            execution: mokmok_protocol::ExecutionID::from_uuid(uuid::Uuid::nil()),
            patch: patch.to_owned(),
            expect: None,
        }
    }

    fn checkout(scratch: &Scratch) -> PathBuf {
        let path = scratch.0.join("checkout");
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn plan_applies_a_patch_to_one_file() {
        let scratch = Scratch::new("apply");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\ntwo\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n",
            ))
            .unwrap();

        let ready = match plan {
            Plan::Ready(ready) => ready,
            Plan::Refused(reason) => panic!("expected Ready, got Refused: {reason}"),
        };
        assert_eq!(ready.relative, "x.rs");
        assert!(std::str::from_utf8(&ready.bytes).unwrap().contains("three"));
    }

    #[test]
    fn plan_refuses_a_two_file_patch() {
        let scratch = Scratch::new("two-files");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "x\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n x\n-y\n--- a/y.rs\n+++ b/y.rs\n@@ -1,1 +1,1 @@\n a\n-b\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected a refusal");
        };
        assert!(
            reason.contains("exactly one file") || reason.contains("more than one"),
            "refusal must name the two-file rule: {reason}"
        );
    }

    #[test]
    fn plan_refuses_a_deletion() {
        let scratch = Scratch::new("delete");
        let checkout = checkout(&scratch);
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n x\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("delete"), "{reason}");
    }

    #[test]
    fn plan_creates_a_file_that_does_not_exist() {
        let scratch = Scratch::new("create");
        let checkout = checkout(&scratch);
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- /dev/null\n+++ b/x.rs\n@@ -0,0 +1,1 @@\n+new\n",
            ))
            .unwrap();

        let Plan::Ready(ready) = plan else {
            panic!("expected Ready");
        };
        assert_eq!(ready.relative, "x.rs");
        assert_eq!(std::str::from_utf8(&ready.bytes).unwrap(), "new\n");
    }

    #[test]
    fn plan_refuses_creating_over_an_existing_file() {
        let scratch = Scratch::new("create-over");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "exists\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- /dev/null\n+++ b/x.rs\n@@ -0,0 +1,1 @@\n+new\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(
            reason.contains("exists") || reason.contains("create"),
            "{reason}"
        );
    }

    #[test]
    fn plan_refuses_an_expect_mismatch() {
        let scratch = Scratch::new("expect");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let mut call = edit_call("--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-one\n+two\n");
        call.expect = Some("0".repeat(64));

        let plan = editor.plan(&call).unwrap();
        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("expected"), "{reason}");
    }

    #[test]
    fn plan_refuses_a_non_utf8_file() {
        let scratch = Scratch::new("binary");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), [0xff, 0xfe]).unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n x\n+y\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("UTF-8"), "{reason}");
    }

    #[test]
    fn plan_refuses_a_non_matching_hunk() {
        let scratch = Scratch::new("mismatch");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n two\n-three\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("hunk"), "{reason}");
        assert!(
            reason.contains("hunk 1"),
            "the first hunk is the one the model wrote: {reason}"
        );
    }

    #[test]
    fn plan_accepts_a_checkout_the_operator_reached_through_a_symlink() {
        let scratch = Scratch::new("symlinked-grant");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&checkout, &link).unwrap();
        // The grant names the symlink, which is the path `--allow-write` was handed.
        let config = config(&scratch.0, std::slice::from_ref(&link), &[]);
        let editor = Editor::new(&config, &link, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-one\n+two\n",
            ))
            .unwrap();

        assert!(matches!(plan, Plan::Ready(_)), "{plan:?}");
    }

    #[test]
    fn plan_refuses_an_expect_that_is_not_a_hash() {
        let scratch = Scratch::new("expect-shape");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let mut call = edit_call("--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-one\n+two\n");
        // 64 characters, one of them not hexadecimal.
        call.expect = Some(format!("{}z", "0".repeat(63)));

        let Plan::Refused(reason) = editor.plan(&call).unwrap() else {
            panic!("expected Refused");
        };
        assert!(
            reason.contains("64-character hexadecimal sha256 hash"),
            "{reason}"
        );
    }

    #[test]
    fn plan_accepts_an_uppercase_expect() {
        let scratch = Scratch::new("expect-case");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let mut call = edit_call("--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-one\n+two\n");
        call.expect = Some(ContentHash::of(b"one\n").to_string().to_uppercase());

        assert!(matches!(editor.plan(&call).unwrap(), Plan::Ready(_)));
    }

    #[test]
    fn plan_refuses_a_symlinked_target() {
        let scratch = Scratch::new("symlink");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("real.rs"), "one\n").unwrap();
        std::os::unix::fs::symlink(checkout.join("real.rs"), checkout.join("x.rs")).unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n one\n+two\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("symbolic link"), "{reason}");
    }

    #[test]
    fn plan_refuses_an_over_size_file() {
        let scratch = Scratch::new("oversize");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "x".repeat(MAX_EDIT_FILE_BYTES + 1)).unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n x\n+y\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("bytes"), "{reason}");
    }

    #[test]
    fn plan_allows_a_file_at_the_size_ceiling() {
        let scratch = Scratch::new("at-ceiling");
        let checkout = checkout(&scratch);
        // One line per byte-pair keeps the hunk matching at the exact ceiling.
        std::fs::write(checkout.join("x.rs"), "x\n".repeat(MAX_EDIT_FILE_BYTES / 2)).unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n x\n+y\n",
            ))
            .unwrap();

        assert!(
            matches!(plan, Plan::Ready(_)),
            "a file exactly at the ceiling must plan: {plan:?}"
        );
    }

    #[test]
    fn open_reason_renders_exact_sentences() {
        let not_found = open_reason(
            &std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            "src/x.rs",
            std::path::Path::new("/tmp/missing"),
            "the parent directory could not be opened",
        );
        assert_eq!(
            not_found,
            "/tmp/missing does not exist; create the parent directory with a command".to_owned(),
        );

        let symlink = open_reason(
            &std::io::Error::from_raw_os_error(nix::errno::Errno::ELOOP as i32),
            "src/x.rs",
            std::path::Path::new("/tmp/link"),
            "the parent directory could not be opened",
        );
        assert_eq!(
            symlink,
            "a directory on the way to src/x.rs is a symbolic link, and this tool never follows \
             one"
            .to_owned(),
        );

        let other = open_reason(
            &std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            "src/x.rs",
            std::path::Path::new("/tmp/dir"),
            "/tmp/dir could not be opened",
        );
        assert!(
            other.contains("/tmp/dir could not be opened") && other.contains("denied"),
            "{other}"
        );
    }

    #[test]
    fn plan_refuses_a_read_only_root() {
        let scratch = Scratch::new("read-only");
        let checkout = checkout(&scratch);
        std::fs::write(checkout.join("x.rs"), "one\n").unwrap();
        let config = config(&scratch.0, &[], std::slice::from_ref(&checkout));
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n one\n+two\n",
            ))
            .unwrap();

        let Plan::Refused(reason) = plan else {
            panic!("expected Refused");
        };
        assert!(reason.contains("granted"), "{reason}");
    }

    #[test]
    fn commit_writes_the_bytes_and_preserves_the_mode() {
        let scratch = Scratch::new("commit");
        let checkout = checkout(&scratch);
        let target = checkout.join("x.rs");
        std::fs::write(&target, "one\ntwo\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n",
            ))
            .unwrap();
        let Plan::Ready(ready) = plan else {
            panic!("expected Ready");
        };

        let report = editor.commit(&ready).unwrap();
        assert!(
            matches!(report.outcome, EditOutcome::Applied),
            "report was not Applied: {}",
            report.observation
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\nthree\n");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[test]
    fn commit_returns_stale_when_the_file_changes() {
        let scratch = Scratch::new("stale");
        let checkout = checkout(&scratch);
        let target = checkout.join("x.rs");
        std::fs::write(&target, "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n one\n+two\n",
            ))
            .unwrap();
        let Plan::Ready(ready) = plan else {
            panic!("expected Ready");
        };

        std::fs::write(&target, "changed\n").unwrap();
        let report = editor.commit(&ready).unwrap();
        assert!(matches!(report.outcome, EditOutcome::Stale { .. }));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "changed\n");
    }

    #[test]
    fn commit_refuses_a_file_that_disappeared_since_the_plan() {
        let scratch = Scratch::new("disappeared");
        let checkout = checkout(&scratch);
        let target = checkout.join("x.rs");
        std::fs::write(&target, "one\n").unwrap();
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n one\n+two\n",
            ))
            .unwrap();
        let Plan::Ready(ready) = plan else {
            panic!("expected Ready");
        };
        assert!(!ready.creates, "the plan read the file");
        std::fs::remove_file(&target).unwrap();

        let report = editor.commit(&ready).unwrap();
        let EditOutcome::Refused { reason } = &report.outcome else {
            panic!("expected Refused, got {:?}", report.outcome);
        };
        assert!(reason.contains("disappeared"), "{reason}");
        assert!(
            !target.exists(),
            "a file the patch does not create stays gone"
        );
    }

    #[test]
    fn commit_creates_the_file_a_patch_names_as_absent() {
        let scratch = Scratch::new("create-commits");
        let checkout = checkout(&scratch);
        let config = config(&scratch.0, std::slice::from_ref(&checkout), &[]);
        let editor =
            Editor::new(&config, &checkout, Arc::new(FixedClock::new(Moment::EPOCH))).unwrap();

        let plan = editor
            .plan(&edit_call(
                "--- /dev/null\n+++ b/x.rs\n@@ -0,0 +1,1 @@\n+new\n",
            ))
            .unwrap();
        let Plan::Ready(ready) = plan else {
            panic!("expected Ready");
        };
        assert!(ready.creates, "the plan read no file");

        let report = editor.commit(&ready).unwrap();
        assert!(
            matches!(report.outcome, EditOutcome::Applied),
            "report was not Applied: {}",
            report.observation
        );
        let target = checkout.join("x.rs");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "mode was {mode:o}");
    }

    /// `O_PATH` carries `renameat` and refuses `fsync`, which is the one way to fail the directory
    /// sync with the bytes already in place.
    #[test]
    fn a_directory_sync_that_fails_after_the_rename_is_not_a_refusal() {
        let scratch = Scratch::new("landed-unsynced");
        let target = scratch.0.join("x.rs");
        std::fs::write(&target, "one\n").unwrap();
        let dir = nix::fcntl::openat2(
            nix::fcntl::AT_FDCWD,
            &scratch.0,
            nix::fcntl::OpenHow::new()
                .flags(OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
                .mode(Mode::empty())
                .resolve(nix::fcntl::ResolveFlag::RESOLVE_NO_SYMLINKS),
        )
        .unwrap();

        let failure = write_atomic(
            &dir,
            OsStr::new("x.rs"),
            ".mokmok-edit-rehearsal",
            b"two\n",
            0o644,
            false,
        )
        .expect_err("a directory sync fails on an O_PATH descriptor");

        assert!(
            matches!(failure, WriteFailure::LandedUnsynced(_)),
            "{failure:?}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "two\n");
    }

    /// A directory on the `newpath` side is what a `renameat` fails on, and the staging file has to
    /// go with it or the refusal leaves a copy of the post-image in the operator's checkout.
    #[test]
    fn a_rename_that_fails_leaves_no_staging_file() {
        let scratch = Scratch::new("rename-fails");
        std::fs::create_dir(scratch.0.join("sub")).unwrap();
        let dir = mokmok_sandbox::open_resolved_dir(&scratch.0).unwrap();

        let failure = write_atomic(
            &dir,
            OsStr::new("sub"),
            ".mokmok-edit-rehearsal",
            b"two\n",
            0o644,
            false,
        )
        .expect_err("renaming a file over a directory fails");

        assert!(
            matches!(failure, WriteFailure::NothingWritten(_)),
            "{failure:?}"
        );
        let mut left: Vec<String> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["sub".to_owned()], "the staging file is gone");
    }

    /// A patch that creates a file takes the target's name only while the target is absent, so a
    /// writer that arrived first keeps its bytes.
    #[test]
    fn a_create_refuses_a_target_that_appeared() {
        let scratch = Scratch::new("create-race");
        let target = scratch.0.join("x.rs");
        std::fs::write(&target, "theirs\n").unwrap();
        let dir = mokmok_sandbox::open_resolved_dir(&scratch.0).unwrap();

        let failure = write_atomic(
            &dir,
            OsStr::new("x.rs"),
            ".mokmok-edit-rehearsal",
            b"mine\n",
            0o644,
            true,
        )
        .expect_err("a create must not replace a file that appeared");

        let WriteFailure::NothingWritten(error) = failure else {
            panic!("expected NothingWritten");
        };
        assert!(
            error
                .to_string()
                .contains("appeared while the edit was being written"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "theirs\n");
        let mut left: Vec<String> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["x.rs".to_owned()], "the staging file is gone");
    }
}

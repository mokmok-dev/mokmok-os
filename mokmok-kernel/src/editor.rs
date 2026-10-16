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

use mokmok_agent_runtime::{
    ArtifactRef, EditCall, EditOutcome, EditReport, Editor as EditorPort, Plan, Ready, edit,
};
use mokmok_artifact::{BlobStore, ContentHash};
use mokmok_patch::{ApplyError, Label, ParseError, Patch, Placed};
use mokmok_protocol::Moment;
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode;

use crate::authority::Authority;
use crate::config::{Config, CrashPoint};
use crate::error::Error;

/// A target file may not be larger than this; a larger file is refused rather than truncated.
const MAX_EDIT_FILE_BYTES: usize = 4 * 1024 * 1024;

/// The adapter that turns one patch into one atomic file write.
pub struct Editor {
    config: Config,
    checkout: PathBuf,
    store: BlobStore,
    at: Moment,
}

impl Editor {
    /// Opens the editor for one checkout and moment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Artifact`] when the blob store cannot be opened, and [`Error::Io`] when
    /// the checkout cannot be resolved.
    pub fn new(
        config: &Config,
        checkout: &Path,
        at: Moment,
    ) -> Result<Self, Error> {
        Ok(Self {
            config: config.clone(),
            checkout: mokmok_sandbox::resolve(checkout)?,
            store: BlobStore::open(&config.artifacts)?,
            at,
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
                format!("hunk {hunk} starts at 0 in a file that exists")
            },
            PatchError::Apply(ApplyError::NotFound { hunk }) => {
                format!("hunk {hunk} does not match the file from its stated line forward")
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
        let authority = Authority::new(&self.config, self.at)?;
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
                    parent,
                    &format!("{relative} could not be opened"),
                )));
            },
        };

        if let Err(denial) = authority.may_write_at(&target, self.at) {
            return Ok(Plan::Refused(format!(
                "{relative} is not under a directory the operator granted for writing, so nothing \
                 was written: {denial}"
            )));
        }

        let (before_bytes, _mode) = match read_target(&parent_fd, name, &relative) {
            Ok(Some(value)) => value,
            Ok(None) => {
                if !matches!(section.source, Label::Absent) {
                    return Ok(Plan::Refused(format!(
                        "{relative} does not exist; create it with a command before patching it"
                    )));
                }
                (Vec::new(), DEFAULT_MODE)
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
            let actual = before_hash.to_string();
            if actual != *expected {
                return Ok(Plan::Refused(format!(
                    "{relative} is {} bytes (sha256 {actual}), not the {}-byte file (sha256 \
                     {expected}) the call expected",
                    before.bytes, before.bytes,
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
                    open_reason(&error, parent, "the parent directory could not be opened"),
                ));
            },
        };

        let (current_bytes, mode) = match read_target(&parent_fd, name, &ready.relative) {
            Ok(Some(value)) => value,
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

        let tmp_name = format!(".mokmok-edit-{}-{}", std::process::id(), ready.after.hash);
        match write_atomic(&parent_fd, name, &tmp_name, &ready.bytes, mode) {
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
            Err(error) => Ok(Self::refused_report(
                &ready.relative,
                format!("the write did not land ({error}), so nothing was written"),
            )),
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
    path: &Path,
    fallback: &str,
) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!(
            "{} does not exist; create the parent directory with a command",
            path.display()
        )
    } else if error.raw_os_error() == Some(nix::errno::Errno::ELOOP as i32) {
        "the target is a symbolic link, and this tool never follows one".to_owned()
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

fn write_atomic(
    parent_fd: &OwnedFd,
    name: &OsStr,
    tmp_name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), io::Error> {
    let mode = Mode::from_bits_truncate(mode & 0o777);
    let tmp = match openat(
        parent_fd,
        tmp_name,
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_NOFOLLOW,
        mode,
    ) {
        Ok(fd) => fd,
        Err(error) => return Err(io::Error::from(error)),
    };

    let mut tmp = File::from(tmp);
    if let Err(error) = tmp.write_all(bytes) {
        let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);
        return Err(error);
    }
    if let Err(error) = tmp.sync_all() {
        let _ = nix::unistd::unlinkat(parent_fd, tmp_name, nix::unistd::UnlinkatFlags::NoRemoveDir);
        return Err(error);
    }
    drop(tmp);

    nix::fcntl::renameat(parent_fd, tmp_name, parent_fd, name).map_err(io::Error::from)?;
    nix::unistd::fsync(parent_fd).map_err(io::Error::from)?;

    Ok(())
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
}

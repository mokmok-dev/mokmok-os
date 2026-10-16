use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access as _, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible as _, PathBeneath,
    Ruleset, RulesetAttr as _, RulesetCreated, RulesetCreatedAttr as _,
};
use nix::fcntl::{AT_FDCWD, OFlag, OpenHow, ResolveFlag, openat2};
use nix::sys::stat::Mode;

use crate::capability::{Access, Exposure, resolve};
use crate::limits::Limits;
use crate::request::Request;
use crate::run::Failure;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Plan {
    /// What the caller passed, which becomes `argv[0]`.
    pub program: text::Text,
    /// The resolved program, which is the object the program's own rule names.
    pub expose: text::Text,
    pub args: Vec<text::Text>,
    pub workspace: text::Text,
    pub exposures: Vec<Grant>,
    pub limits: Limits,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Grant {
    pub path: text::Text,
    pub access: Access,
}

impl Plan {
    /// Records only resolved paths, so the gate, the plan, and the ruleset name the same object.
    #[must_use]
    pub fn of(
        request: &Request,
        expose: &Path,
        workspace: &Path,
        exposures: &[Exposure],
    ) -> Self {
        Self {
            program: text::Text::from(request.program.as_path()),
            expose: text::Text::from(expose),
            args: request
                .args
                .iter()
                .map(|arg| text::Text::from(arg.as_os_str()))
                .collect(),
            workspace: text::Text::from(workspace),
            exposures: exposures
                .iter()
                .map(|exposure| Grant {
                    path: text::Text::from(exposure.as_path()),
                    access: exposure.access(),
                })
                .collect(),
            limits: request.limits,
        }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;

        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub fn write(
        path: &Path,
        plan: &Self,
    ) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(plan).map_err(io::Error::other)?;

        // The plan names the paths a workload may reach, so it is the owner's to read.
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
            .write_all(&bytes)
    }
}

/// The ruleset the confiner restricts itself with.
///
/// Landlock V3 is required because V3 mediates `TRUNCATE`, which V1 leaves as an escape. The same
/// ruleset handles `AccessNet::from_all(ABI::V4)` and adds no network rule, so `BindTcp` and
/// `ConnectTcp` are denied to every workload. `AccessNet` has no UDP right and no `AF_UNIX` right,
/// so a workload can still send UDP to a literal address and reach a Unix socket.
///
/// The floor is Linux 6.7 (`ABI::V4`). `CompatLevel::HardRequirement` makes a kernel below it
/// refuse to build the ruleset rather than silently drop the network restriction, which is the
/// fail-closed direction and the reason [`LimitsOnly`](crate::LimitsOnly) exists.
pub fn ruleset(plan: &Plan) -> Result<RulesetCreated, Failure> {
    let mut by_path: BTreeMap<PathBuf, BitFlags<AccessFs>> = BTreeMap::new();

    // Two names for one path become one rule with the union of their rights: the program's own
    // file is exposed whether or not a capability also names it.
    merge(
        &mut by_path,
        plan.expose.to_path_buf(),
        Access::READ_EXECUTE,
    );
    merge(
        &mut by_path,
        plan.workspace.to_path_buf(),
        Access::READ_WRITE_EXECUTE,
    );
    for exposure in &plan.exposures {
        merge(&mut by_path, exposure.path.to_path_buf(), exposure.access);
    }

    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V3))
        .map_err(ruleset_failure)?
        .handle_access(AccessNet::from_all(ABI::V4))
        .map_err(ruleset_failure)?
        .create()
        .map_err(ruleset_failure)?;

    for (path, mut access) in by_path {
        if !path.is_dir() {
            access &= AccessFs::from_file(ABI::V3);
        }

        // The plan records resolved paths, so a symlink anywhere in one means the object moved
        // after the plan was written; refusing it is the fail-closed direction.
        let opened = open_resolved(&path)
            .map_err(|error| Failure::Path(format!("{}: {error}", path.display())))?;

        ruleset = ruleset
            .add_rule(PathBeneath::new(opened, access))
            .map_err(ruleset_failure)?;
    }

    Ok(ruleset)
}

/// Opens a resolved path atomically, with no symlink resolution, so a symlink planted after the
/// plan was written cannot move the rule to an object the gate never covered. Refuses with
/// `ELOOP` when any path component is a symbolic link.
///
/// # Errors
///
/// Returns the `openat2` error: `ELOOP` for a symlink, `ENOENT` when the path is missing, and the
/// underlying kernel error for any other refusal.
pub fn open_resolved(path: &Path) -> io::Result<OwnedFd> {
    openat2(
        AT_FDCWD,
        path,
        OpenHow::new()
            .flags(OFlag::O_PATH | OFlag::O_CLOEXEC)
            .mode(Mode::empty())
            .resolve(ResolveFlag::RESOLVE_NO_SYMLINKS),
    )
    .map_err(io::Error::from)
}

/// Opens a resolved directory atomically, with no symlink resolution, so a symlink planted after
/// planning cannot move the directory the caller will read and write through. Refuses with
/// `ENOTDIR` when the path is not a directory and with `ELOOP` when any component is a symlink.
///
/// The descriptor is opened read-only rather than `O_PATH` so the caller may `fsync` it after an
/// atomic rename, which `O_PATH` refuses with `EBADF`.
///
/// # Errors
///
/// Returns the `openat2` error: `ELOOP` for a symlink, `ENOTDIR` when the path is not a directory,
/// `ENOENT` when it is missing, and the underlying kernel error otherwise.
pub fn open_resolved_dir(path: &Path) -> io::Result<OwnedFd> {
    openat2(
        AT_FDCWD,
        path,
        OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
            .mode(Mode::empty())
            .resolve(ResolveFlag::RESOLVE_NO_SYMLINKS),
    )
    .map_err(io::Error::from)
}

fn merge(
    by_path: &mut BTreeMap<PathBuf, BitFlags<AccessFs>>,
    path: PathBuf,
    access: Access,
) {
    *by_path.entry(path).or_default() |= rights(access);
}

/// Builds the ruleset [`Confined::new`](crate::Confined::new) probes with, and drops it.
///
/// Constructing a ruleset is what probes Landlock, and this is the same code the confiner runs,
/// so the probe and the confiner cannot drift.
pub fn probe(
    program: &Path,
    workspace: &Path,
) -> Result<(), Failure> {
    // The confiner opens every rule path with no symlink resolution, and a real plan records
    // resolved paths, so the probe hands it resolved inputs too.
    let expose = resolve(program).unwrap_or_else(|_| program.to_path_buf());
    let resolved = resolve(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let plan = Plan {
        program: text::Text::from(program),
        expose: text::Text::from(expose.as_path()),
        args: Vec::new(),
        workspace: text::Text::from(resolved.as_path()),
        exposures: Vec::new(),
        limits: Limits::default(),
    };

    ruleset(&plan).map(drop)
}

fn rights(access: Access) -> BitFlags<AccessFs> {
    let mut rights = BitFlags::<AccessFs>::EMPTY;

    if access.covers(Access::READ) {
        rights |= AccessFs::from_read(ABI::V3);
    }
    if access.covers(Access::WRITE) {
        rights |= AccessFs::from_write(ABI::V3);
    }
    if access.covers(Access::EXECUTE) {
        rights |= AccessFs::Execute;
    }

    rights
}

fn ruleset_failure(error: impl std::fmt::Display) -> Failure {
    Failure::Ruleset(error.to_string())
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Written as the confiner's first write of all, before anything can fail.
    Prepared,
    /// Written after `restrict_self` reports `FullyEnforced`.
    Confined,
    /// Written immediately before the exec call.
    Attempted,
    /// The workload never ran.
    Refused { failure: Failure },
}

/// The states a reader has to name when the confiner stopped before the exec attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reached {
    /// The confiner wrote its first record and then stopped.
    Prepared,
    /// The confiner enforced the ruleset and then stopped.
    Confined,
}

/// The status file, or `None` when it is absent or does not parse.
///
/// `None` means the confiner never reached its first write.
#[must_use]
pub fn read_status(path: &Path) -> Option<Status> {
    let bytes = std::fs::read(path).ok()?;

    serde_json::from_slice(&bytes).ok()
}

const STATUS_BYTES: usize = 4096;
const PADDING: [u8; STATUS_BYTES] = [b' '; STATUS_BYTES];

/// The confiner's write end of the status file, held open across `restrict_self`.
pub struct StatusFile {
    file: File,
}

impl StatusFile {
    /// Opens the file `0600`, truncating, so a refusal can still be written after the ruleset is
    /// enforced.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;

        Ok(Self { file })
    }

    pub fn prepared(&mut self) -> io::Result<()> {
        self.record(&Status::Prepared)
    }

    pub fn confined(&mut self) -> io::Result<()> {
        self.record(&Status::Confined)
    }

    pub fn attempted(&mut self) -> io::Result<()> {
        self.record(&Status::Attempted)
    }

    pub fn refuse(
        &mut self,
        failure: &Failure,
    ) -> io::Result<()> {
        self.record(&Status::Refused {
            failure: failure.clone(),
        })
    }

    /// Overwrites a fixed-size record rather than truncating. Landlock V3 mediates `ftruncate`,
    /// and `ctl/` sits outside every rule the plan grants, so after `restrict_self` an overwrite
    /// is the only write that still goes through. The pad is JSON whitespace.
    fn record(
        &mut self,
        status: &Status,
    ) -> io::Result<()> {
        let encoded = serde_json::to_vec(status).unwrap_or_default();
        let padding = STATUS_BYTES.saturating_sub(encoded.len());

        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&encoded)?;
        self.file.write_all(&PADDING[..padding])?;

        Ok(())
    }
}

pub mod text {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::path::{Path, PathBuf};

    use serde::Deserialize as _;

    /// An `OsString` that survives the wire: a JSON string when it is UTF-8, and an array of bytes
    /// when it is not, which is how `serde_json` writes bytes.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Text(OsString);

    impl Text {
        #[must_use]
        pub fn os(&self) -> &OsStr {
            &self.0
        }

        #[must_use]
        pub fn to_path_buf(&self) -> PathBuf {
            PathBuf::from(&self.0)
        }
    }

    impl From<&Path> for Text {
        fn from(path: &Path) -> Self {
            Self(path.as_os_str().to_os_string())
        }
    }

    impl From<&OsStr> for Text {
        fn from(value: &OsStr) -> Self {
            Self(value.to_os_string())
        }
    }

    impl serde::Serialize for Text {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serialize(&self.0, serializer)
        }
    }

    impl<'de> serde::Deserialize<'de> for Text {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserialize(deserializer).map(Self)
        }
    }

    /// The `with` hook for an `OsString` or `PathBuf` field.
    pub fn serialize<S, T>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
        T: AsRef<OsStr> + ?Sized,
    {
        match value.as_ref().to_str() {
            Some(text) => serializer.serialize_str(text),
            None => serializer.serialize_bytes(value.as_ref().as_bytes()),
        }
    }

    /// The `with` hook for an `OsString` or `PathBuf` field.
    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>,
        T: From<OsString>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Bytes(Vec<u8>),
        }

        Ok(match Raw::deserialize(deserializer)? {
            Raw::Text(text) => T::from(OsString::from(text)),
            Raw::Bytes(bytes) => T::from(OsString::from_vec(bytes)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Plan, Status, StatusFile, probe, read_status, rights, ruleset};
    use crate::capability::Access;
    use crate::limits::Limits;
    use crate::request::Request;
    use crate::run::Failure;
    use landlock::{ABI, Access as _, AccessFs, BitFlags};
    use mokmok_protocol::ExecutionID;
    use std::path::Path;
    use uuid::timestamp::{Timestamp, context::NoContext};

    fn plan(
        program: &Path,
        workspace: &Path,
    ) -> Plan {
        let mut request = Request::new(ExecutionID::new(Timestamp::now(NoContext)), program);
        request.limits = Limits::default();

        Plan::of(&request, program, workspace, &request.exposures)
    }

    #[test]
    fn rights_follow_the_three_components() {
        assert_eq!(rights(Access::READ), AccessFs::from_read(ABI::V3));
        assert_eq!(rights(Access::WRITE), AccessFs::from_write(ABI::V3));
        assert_eq!(
            rights(Access::EXECUTE),
            BitFlags::<AccessFs>::from(AccessFs::Execute),
        );
        assert_eq!(
            rights(Access::READ_WRITE_EXECUTE),
            AccessFs::from_all(ABI::V3),
        );
    }

    #[test]
    fn a_path_that_is_not_there_is_refused() {
        let missing = std::env::temp_dir().join("mokmok-plan-missing-path");

        let built = ruleset(&plan(&missing, &missing));

        assert!(
            matches!(built, Err(Failure::Path(_))),
            "a path that is not there was accepted: {built:?}",
        );
    }

    #[test]
    fn the_probe_refuses_a_workspace_that_is_not_there() {
        let missing = std::env::temp_dir().join("mokmok-probe-missing-path");

        let probed = probe(&missing, &missing);

        assert!(matches!(probed, Err(Failure::Path(_))), "got {probed:?}");
    }

    #[test]
    fn a_path_through_a_symlink_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("mokmok-plan-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("target");
        std::fs::write(&target, b"x").unwrap();
        let link = directory.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut request = Request::new(
            ExecutionID::new(Timestamp::now(NoContext)),
            directory.join("absent-program"),
        );
        request.limits = Limits::default();
        let exposures = [crate::capability::Exposure::read(&link)];
        let built = ruleset(&Plan::of(&request, &target, &directory, &exposures));

        assert!(
            matches!(built, Err(Failure::Path(_))),
            "a path that resolves through a symlink was accepted: {built:?}",
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn the_program_and_the_workspace_are_covered_without_a_capability() {
        let directory = std::env::temp_dir().join(format!("mokmok-plan-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let program = directory.join("program");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();

        let built = ruleset(&plan(&program, &directory));

        assert!(built.is_ok(), "the plan builds: {built:?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn the_status_file_round_trips_every_state() {
        let path = std::env::temp_dir().join(format!(
            "mokmok-status-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut status = StatusFile::create(&path).unwrap();

        status.prepared().unwrap();
        assert!(matches!(read_status(&path), Some(Status::Prepared)));

        status.confined().unwrap();
        assert!(matches!(read_status(&path), Some(Status::Confined)));

        status.attempted().unwrap();
        assert!(matches!(read_status(&path), Some(Status::Attempted)));

        let failure = Failure::Session("no session".into());
        status.refuse(&failure).unwrap();
        let reported = read_status(&path).expect("the refusal parses");

        assert!(matches!(
            reported,
            Status::Refused { failure: Failure::Session(ref message) } if message == "no session",
        ));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_unknown_access_bit_is_refused_at_the_wire() {
        let json = r#"{"program":"/bin/sh","expose":"/bin/sh","args":[],"workspace":"/tmp","exposures":[{"path":"/x","access":8}],"limits":{"address_space":null,"cpu":null,"file_size":null,"open_files":null,"output":1024}}"#;
        let parsed: Result<Plan, _> = serde_json::from_str(json);

        assert!(
            parsed.is_err(),
            "a right the crate does not know was accepted"
        );

        let zero = json.replace("\"access\":8", "\"access\":0");
        assert!(
            serde_json::from_str::<Plan>(&zero).is_err(),
            "no rights were accepted",
        );
    }

    #[test]
    fn a_file_rule_drops_the_directory_only_rights() {
        let directory =
            std::env::temp_dir().join(format!("mokmok-plan-file-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("file");
        std::fs::write(&file, b"x").unwrap();
        let mut request = Request::new(
            ExecutionID::new(Timestamp::now(NoContext)),
            directory.join("absent-program"),
        );
        request.exposures = vec![crate::capability::Exposure::read_write(&file)];
        let built = ruleset(&Plan::of(&request, &file, &directory, &request.exposures));

        // `ReadDir` and `MakeDir` are directory-only and Landlock rejects them on a file rule.
        assert!(built.is_ok(), "a file rule builds: {built:?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

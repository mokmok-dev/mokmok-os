use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access as _, AccessFs, BitFlags, CompatLevel, Compatible as _, PathBeneath, PathFd,
    Ruleset, RulesetAttr as _, RulesetCreated, RulesetCreatedAttr as _,
};

use crate::capability::Access;
use crate::limits::Limits;
use crate::request::Request;
use crate::run::Failure;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Plan {
    pub program: text::Text,
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
    #[must_use]
    pub fn of(
        request: &Request,
        workspace: &Path,
    ) -> Self {
        Self {
            program: text::Text::from(request.program.as_path()),
            args: request
                .args
                .iter()
                .map(|arg| text::Text::from(arg.as_os_str()))
                .collect(),
            workspace: text::Text::from(workspace),
            exposures: request
                .exposures
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
/// Landlock V3 is required because V3 mediates `TRUNCATE`, which V1 leaves as an escape. A kernel
/// older than 6.2 fails to build this ruleset, which is the fail-closed direction and the reason
/// [`LimitsOnly`](crate::LimitsOnly) exists.
pub fn ruleset(plan: &Plan) -> Result<RulesetCreated, Failure> {
    let mut by_path: BTreeMap<PathBuf, BitFlags<AccessFs>> = BTreeMap::new();

    // Two names for one path become one rule with the union of their rights: the program's own
    // file is exposed whether or not a capability also names it.
    merge(
        &mut by_path,
        plan.program.to_path_buf(),
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
        .create()
        .map_err(ruleset_failure)?;

    for (path, mut access) in by_path {
        if !path.exists() {
            tracing::debug!(
                path = %path.display(),
                "skipping a rule for a path that is not there",
            );

            continue;
        }

        if !path.is_dir() {
            access &= AccessFs::from_file(ABI::V3);
        }

        let rule = PathFd::new(&path).map_err(ruleset_failure)?;

        ruleset = ruleset
            .add_rule(PathBeneath::new(rule, access))
            .map_err(ruleset_failure)?;
    }

    Ok(ruleset)
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
    let plan = Plan {
        program: text::Text::from(program),
        args: Vec::new(),
        workspace: text::Text::from(workspace),
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
    /// Written before the confiner does anything that can fail.
    Reserved,
    /// The workload never ran.
    Refused { failure: Failure },
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

    pub fn reserved(&mut self) -> io::Result<()> {
        self.record(&Status::Reserved)
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
    use super::{Plan, Status, StatusFile, read_status, rights, ruleset};
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

        Plan::of(&request, workspace)
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
    fn a_nonexistent_path_is_skipped() {
        let missing = std::env::temp_dir().join("mokmok-plan-missing-path");

        let built = ruleset(&plan(&missing, &missing));

        assert!(
            built.is_ok(),
            "a plan whose paths are absent still builds: {built:?}",
        );
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
    fn the_status_file_round_trips_both_states() {
        let path = std::env::temp_dir().join(format!(
            "mokmok-status-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut status = StatusFile::create(&path).unwrap();

        status.reserved().unwrap();
        assert!(matches!(read_status(&path), Some(Status::Reserved)));

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
        let json = r#"{"program":"/bin/sh","args":[],"workspace":"/tmp","exposures":[{"path":"/x","access":8}],"limits":{"address_space":null,"cpu":null,"file_size":null,"open_files":null,"output":1024}}"#;
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
        let built = ruleset(&Plan::of(&request, &directory));

        // `ReadDir` and `MakeDir` are directory-only and Landlock rejects them on a file rule.
        assert!(built.is_ok(), "a file rule builds: {built:?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

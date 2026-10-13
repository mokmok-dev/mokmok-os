//! Runs one tool under the sandbox's confinement and stores what it produced.
//!
//! A run is dispatched only after its intent is durable, and what it produced is written where a
//! reader can find it before the fact that names it is committed: the bytes go into the artifact
//! store first, and the log entry that references them follows in the same transaction as the
//! settlement it belongs to. A crash between those two leaves an unreferenced blob, which the
//! collector takes, never a reference to bytes that were never written.
//!
//! The result's bytes stay outside the event log. What the log keeps is a content hash and a byte
//! count, so a large answer costs the log the same as a small one.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use mokmok_artifact::BlobStore;
use mokmok_capability::{Denial, Name};
use mokmok_protocol::{ExecutionID, Moment};
use mokmok_sandbox::{
    Capability, Completed, Confined, Environment, Error as SandboxError, Exposure, Failure,
    Outcome, Refusal, Request, Sandbox, Started,
};
use mokmok_state_machine::Resolution;

use crate::authority::Authority;
use crate::config::Config;
use crate::error::Error;

/// Where one execution's bytes ended up.
///
/// The bytes live in the artifact store; this is the reference that keeps them alive. The hash is
/// spelled as lowercase hex rather than carried as the store's own type, because this value is a
/// durable record and its reader is a log entry, not a caller holding a `ContentHash`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Blob {
    pub hash: String,
    pub bytes: u64,
    /// Whether capture stopped before the workload stopped writing.
    pub truncated: bool,
}

/// What one execution produced.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExecutionResult {
    /// The exit code, when the workload exited on its own.
    pub code: Option<i32>,
    /// The signal number, when a signal ended it.
    pub signal: Option<i32>,
    /// Whether the confiner reported reaching the exec call, which is all a supervisor can observe.
    pub attempted: bool,
    pub stdout: Blob,
    pub stderr: Blob,
    /// Registered artifacts, each by the workspace-relative path its caller declared.
    pub artifacts: Vec<(String, Blob)>,
}

/// What one execution needs from the kernel.
#[derive(Clone, Debug)]
pub struct Plan {
    pub execution: ExecutionID,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// How the workload's environment is built; `Exact` is what the tool path uses, so the daemon's
    /// own variables do not reach a model-driven shell.
    pub environment: Environment,
    pub deadline: Duration,
    /// Workspace-relative paths to register after the run, when they exist.
    pub artifacts: Vec<PathBuf>,
    /// Every path the workload may reach, as the operator granted it.
    pub exposures: Vec<PathBuf>,
}

/// How one attempt at the tool ended.
#[derive(Debug)]
pub enum Ran {
    /// Nothing was created and no process ran.
    Refused(Refused),
    Settled {
        resolution: Resolution,
        result: ExecutionResult,
    },
}

/// Why an attempt never reached the workload.
#[derive(Debug)]
pub enum Refused {
    /// The authority did not allow the invocation.
    Authority(Denial),
    /// The sandbox refused the plan before it created anything.
    Sandbox(Refusal),
    /// The confiner refused after it started, so the workload never ran.
    Confined(Failure),
}

/// Runs one tool: decide, confine, execute, store, and report.
///
/// # Errors
///
/// Returns [`Error::InvalidName`] when the program is not a name the policy can carry,
/// [`Error::Artifact`] when the bytes cannot be stored, [`Error::Sandbox`] when the supervisor
/// fails after the workload may have started, and [`Error::Refusal`] is never returned: a refusal
/// is a recorded outcome, not a failure of the kernel.
pub async fn run(
    config: &Config,
    sandbox: &Confined,
    authority: &Authority,
    plan: &Plan,
    at: Moment,
) -> Result<Ran, Error> {
    let tool =
        Name::new(plan.program.to_string_lossy().into_owned()).map_err(Error::InvalidName)?;
    if let Err(denial) = authority.may_invoke(&tool, at) {
        return Ok(Ran::Refused(Refused::Authority(denial)));
    }

    let mut exposures = Vec::new();
    let mut issued = Vec::new();
    for path in &plan.exposures {
        let access = match authority.access_at(path, at) {
            Ok(access) => access,
            Err(denial) => return Ok(Ran::Refused(Refused::Authority(denial))),
        };
        exposures.push(Exposure::path(path, access));
        issued.push(Capability::issued(path, access));
    }

    let mut request = Request::new(plan.execution, &plan.program);
    request.args.clone_from(&plan.args);
    request.environment.clone_from(&plan.environment);
    request.exposures = exposures;
    request.deadline = Some(plan.deadline);
    request.artifacts.clone_from(&plan.artifacts);

    let run = match sandbox.spawn(&request, &issued) {
        Ok(run) => run,
        Err(refusal) => return Ok(Ran::Refused(Refused::Sandbox(refusal))),
    };

    let completed = match run.wait().await {
        Ok(completed) => completed,
        Err(SandboxError::Confine(failure)) => {
            return Ok(Ran::Refused(Refused::Confined(failure)));
        },
        Err(error) => return Err(Error::Sandbox(error)),
    };

    let resolution = resolution_of(completed.outcome());
    // The boundary the crash points exist for: the workload has run, nothing about it is durable
    // yet, and the workspace that would prove what it did is still on disc.
    crate::config::crash(config, crate::config::CrashPoint::AfterDispatch);
    let result = store(config, &completed)?;
    completed.cleanup()?;

    Ok(Ran::Settled { resolution, result })
}

/// How the supervisor's end state maps onto the session's own vocabulary.
#[must_use]
pub const fn resolution_of(outcome: Outcome) -> Resolution {
    match outcome {
        Outcome::Exited(code) => Resolution::Finished { code },
        Outcome::Signalled(signal) => Resolution::Killed { signal },
        Outcome::Overran => Resolution::TimedOut,
    }
}

/// Writes what the workload produced into the artifact store and returns the references.
fn store(
    config: &Config,
    completed: &Completed,
) -> Result<ExecutionResult, Error> {
    let store = BlobStore::open(&config.artifacts)?;
    let mut artifacts = Vec::new();

    for artifact in completed.artifacts() {
        let bytes = std::fs::read(completed.workspace().join(artifact.path()))?;
        artifacts.push((
            artifact.path().to_string_lossy().into_owned(),
            put(&store, &bytes, false)?,
        ));
    }

    Ok(ExecutionResult {
        code: match completed.outcome() {
            Outcome::Exited(code) => Some(code),
            _ => None,
        },
        signal: match completed.outcome() {
            Outcome::Signalled(signal) => Some(signal),
            _ => None,
        },
        attempted: matches!(completed.started(), Started::Attempted),
        stdout: put(
            &store,
            completed.stdout().bytes(),
            completed.stdout().truncated(),
        )?,
        stderr: put(
            &store,
            completed.stderr().bytes(),
            completed.stderr().truncated(),
        )?,
        artifacts,
    })
}

fn put(
    store: &BlobStore,
    bytes: &[u8],
    truncated: bool,
) -> Result<Blob, Error> {
    let hash = store.put(bytes)?;

    Ok(Blob {
        hash: hash.to_string(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        truncated,
    })
}

/// The paths a plan asks the workload to reach: what the operator opened, plus the standard input
/// the sandbox itself needs.
#[must_use]
pub fn exposures(config: &Config) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for path in config
        .allow_path
        .iter()
        .chain(&config.allow_write)
        .chain(std::iter::once(&PathBuf::from(crate::authority::DEV_NULL)))
    {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::{Ran, Refused, resolution_of};
    use mokmok_sandbox::Outcome;
    use mokmok_state_machine::Resolution;

    #[test]
    fn each_end_state_maps_to_the_session_vocabulary() {
        assert_eq!(
            resolution_of(Outcome::Exited(3)),
            Resolution::Finished { code: 3 },
        );
        assert_eq!(
            resolution_of(Outcome::Signalled(9)),
            Resolution::Killed { signal: 9 },
        );
        assert_eq!(resolution_of(Outcome::Overran), Resolution::TimedOut);
    }

    #[test]
    fn a_refusal_is_an_outcome_rather_than_a_failure() {
        let refused: Ran = Ran::Refused(Refused::Authority(mokmok_capability::Denial::new(
            mokmok_capability::DenialReason::NoMatchingRule,
        )));

        assert!(matches!(refused, Ran::Refused(_)));
    }

    #[test]
    fn the_exposure_list_opens_what_the_operator_named_and_the_standard_input() {
        use crate::config::{Args, Command, Config};
        use clap::Parser as _;
        use std::path::PathBuf;

        let data =
            std::env::temp_dir().join(format!("mokmok-kernel-exposures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let args = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            data.to_str().unwrap(),
            "--allow-path",
            "/bin",
            "--allow-write",
            "/bin",
            "--confiner",
            "/bin/sh",
            "submit",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--idempotency-key",
            "018f2f1e-0000-7000-8000-000000000001",
            "--program",
            "/bin/sh",
        ])
        .unwrap();
        assert!(matches!(args.command, Command::Submit(_)));
        let config = Config::resolve(&args).unwrap();

        assert_eq!(
            super::exposures(&config),
            vec![PathBuf::from("/bin"), PathBuf::from("/dev/null")],
        );
        let _ = std::fs::remove_dir_all(&data);
    }
}

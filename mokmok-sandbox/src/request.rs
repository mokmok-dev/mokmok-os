use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use mokmok_protocol::ExecutionID;

use crate::capability::Exposure;
use crate::limits::Limits;

/// How a workload's environment is built from the one its supervisor holds.
///
/// The supervisor's own environment is not a safe default for a confined workload: whatever the
/// daemon holds reaches every workload it starts. [`Environment::Exact`] is the arm that closes
/// that, and a caller handling a credential uses it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Environment {
    /// Adds these to the inherited environment.
    Extend(Vec<(OsString, OsString)>),
    /// Clears the inherited environment and sets exactly what it names.
    Exact(Vec<(OsString, OsString)>),
}

impl Default for Environment {
    fn default() -> Self {
        Self::Extend(Vec::new())
    }
}

/// Everything one execution needs: what to run, what it may reach, and how it is bounded.
#[derive(Clone, Debug)]
pub struct Request {
    pub execution: ExecutionID,
    /// Absolute. The plan exposes this file so the workload can start; a bare name would not
    /// identify what to expose.
    pub program: PathBuf,
    /// Arguments after the program name; `program` becomes `argv[0]`.
    pub args: Vec<OsString>,
    /// How the workload's environment relates to the one this process holds.
    pub environment: Environment,
    /// The paths the caller wants the workload to reach, each covered by an issued capability.
    pub exposures: Vec<Exposure>,
    /// The directory the workload runs in, when it is not the execution's own workspace.
    ///
    /// Absolute, and it must lie inside one of `exposures`; the sandbox neither creates nor
    /// removes it.
    pub workdir: Option<PathBuf>,
    pub limits: Limits,
    /// This one execution's deadline. The agent's budget is a different responsibility and lives
    /// with the agent runtime.
    pub deadline: Option<Duration>,
    /// Paths relative to the workspace, registered after the run if they exist.
    pub artifacts: Vec<PathBuf>,
}

impl Request {
    #[must_use]
    pub fn new(
        execution: ExecutionID,
        program: impl Into<PathBuf>,
    ) -> Self {
        Self {
            execution,
            program: program.into(),
            args: Vec::new(),
            environment: Environment::default(),
            exposures: Vec::new(),
            workdir: None,
            limits: Limits::default(),
            deadline: None,
            artifacts: Vec::new(),
        }
    }
}

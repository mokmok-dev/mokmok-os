use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use mokmok_protocol::ExecutionID;

use crate::capability::Exposure;
use crate::limits::Limits;

/// Everything one execution needs: what to run, what it may reach, and how it is bounded.
#[derive(Clone, Debug)]
pub struct Request {
    pub execution: ExecutionID,
    /// Absolute. The plan exposes this file so the workload can start; a bare name would not
    /// identify what to expose.
    pub program: PathBuf,
    /// Arguments after the program name; `program` becomes `argv[0]`.
    pub args: Vec<OsString>,
    /// Added to the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// The paths the caller wants the workload to reach, each covered by an issued capability.
    pub exposures: Vec<Exposure>,
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
            env: Vec::new(),
            exposures: Vec::new(),
            limits: Limits::default(),
            deadline: None,
            artifacts: Vec::new(),
        }
    }
}

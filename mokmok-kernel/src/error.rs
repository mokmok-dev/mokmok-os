//! Everything the composition root can fail at, in the terms its caller reads.

use std::path::PathBuf;

use mokmok_event_store::{Error as StoreError, PayloadError};
use mokmok_state_machine::{Rejected, Rejection};

use crate::config::ConfigError;

/// A failure of one kernel operation.
///
/// The kernel is the top of the workspace, so this is where the lower crates' failures stop being
/// transport-specific and become something a CLI caller can act on. A refusal of authority is not
/// here: a workload that may not run is a recorded outcome, not a failure of the kernel.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the event log failed")]
    Store(#[from] StoreError),
    #[error("the configuration is not usable: {0}")]
    Config(#[from] ConfigError),
    #[error("the filesystem refused a write the run depends on")]
    Io(#[from] std::io::Error),
    #[error("the artifact store failed")]
    Artifact(#[from] mokmok_artifact::Error),
    #[error("the sandbox refused the execution it was asked for")]
    Refusal(#[from] mokmok_sandbox::Refusal),
    #[error("the execution failed after the workload may have started")]
    Sandbox(#[from] mokmok_sandbox::Error),
    #[error("the event bus refused the connection or the append")]
    Bus(#[from] mokmok_event_bus::Error),
    #[error("the bus server failed: {0}")]
    BusServe(#[from] mokmok_event_bus::ServeError),
    #[error("the bus stopped, so this process has nothing to serve")]
    BusStopped,
    #[error("the bus never answered {0} while this process was willing to wait")]
    BusUnanswered(PathBuf),
    #[error("a notice's topic is not one the bus names")]
    Topic(#[from] mokmok_event_bus::InvalidTopic),
    #[error("a committed payload is not the value it was written as")]
    BusPayload(#[from] mokmok_event_bus::PayloadError),
    #[error("a committed payload is not JSON")]
    NotJson(#[from] serde_json::Error),
    #[error("the batch carries no entry, so there is nothing to commit")]
    Empty,
    #[error("{0} is not a name an event kind can carry")]
    NotAName(&'static str),
    #[error(
        "the log entry at position {position} is not {expected}, so the history cannot be replayed"
    )]
    UnexpectedEntry {
        position: u64,
        expected: &'static str,
    },
    #[error("the recorded history is not a state this build accepts")]
    Rejected(#[from] Rejected),
    #[error("the step the kernel was about to write is one the machines refuse")]
    WriteRefused(Rejection),
    #[error("a stored payload could not be read back")]
    Payload(#[from] PayloadError),
    #[error("another writer moved the stream to version {found} before this append")]
    Conflicted { found: u64 },
    #[error("the session is {0:?}, so it admits no new turn")]
    Closed(&'static str),
    #[error("the command id was already used for a session that does not hold this turn")]
    Divergent,
    #[error("no session {0} is recorded in the log")]
    UnknownSession(uuid::Uuid),
    #[error("{0} is not a name a principal or a tool can carry")]
    InvalidName(#[from] mokmok_capability::InvalidName),
}

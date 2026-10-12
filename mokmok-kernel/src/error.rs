//! Everything the composition root can fail at, in the terms its caller reads.

use mokmok_event_store::{Error as StoreError, PayloadError};
use mokmok_state_machine::Rejected;

use crate::config::ConfigError;

/// A failure of one kernel operation.
///
/// The kernel is the top of the workspace, so this is where the lower crates' failures stop being
/// transport-specific and become something a CLI caller can act on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the event log failed")]
    Store(#[from] StoreError),
    #[error("the configuration is not usable")]
    Config(#[from] ConfigError),
    #[error("the batch carries no fact, so there is nothing to commit")]
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
    #[error("a stored payload could not be read back")]
    Payload(#[from] PayloadError),
}

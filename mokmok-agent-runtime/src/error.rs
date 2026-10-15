//! The one error type every fallible path in the crate answers with.

use mokmok_state_machine::Rejected;

use crate::record::Refusal;

/// A port's own failure, erased because the runtime may not name the type that produced it.
///
/// The kernel's adapters build these; the runtime only carries them to the caller.
pub type Source = Box<dyn std::error::Error + Send + Sync>;

/// Why a step could not complete.
///
/// The port arms carry the adapter's error without naming its type, so no store, socket, sandbox, or
/// index type crosses the port boundary. The last two are the runtime's own: [`Refusal`] is the
/// fold rejecting a record, and [`Rejected`] is a state machine rejecting a fact.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The journal refused a read, a store, or an append.
    #[error("the journal failed: {0}")]
    Journal(#[source] Source),
    /// The provider failed to answer.
    #[error("the provider failed: {0}")]
    Provider(#[source] Source),
    /// The tools failed to report.
    #[error("the tools failed: {0}")]
    Tool(#[source] Source),
    /// The finder failed to answer.
    #[error("the finder failed: {0}")]
    Finder(#[source] Source),
    /// The fold refused a record.
    #[error("{0}")]
    Refused(#[from] Refusal),
    /// A state machine refused a fact.
    #[error("{0}")]
    Rejected(#[from] Rejected),
    /// The session is not waiting for a human line.
    #[error("the session is not waiting for a human line")]
    NotAwaiting,
    /// The human line is longer than the log holds.
    #[error("the line is {found} bytes, and the log holds {limit}")]
    TooMuch { limit: usize, found: usize },
}

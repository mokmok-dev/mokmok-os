//! A conversation with the constrained agent, drawn on a terminal, in as many sessions as the human
//! holds.
//!
//! The screen is the durable [`Transcript`]s of the sessions this process holds, plus the one
//! answer a focused step is still streaming: the crate keeps the lines being typed, how far back
//! each transcript is scrolled, which session the human is in, the pane he opened, and the keys a
//! step deferred, and everything else the screen shows and the loop does next comes from the folds.
//! [`run`] drives the same kernel loop `mokmokd agent run` drives, one bounded step at a time, for
//! whichever session's turn it is.
//!
//! [`Transcript`]: mokmok_agent_runtime::Transcript

mod bus;
mod core;
mod markdown;
mod port;
mod terminal;

use mokmok_protocol::SessionID;
use mokmok_state_machine::Ending;

pub use crate::bus::BusRoster;
pub use crate::core::run;
pub use crate::port::{
    Engine, Entry, Event, Key, Line, Link, Listing, Roster, Row, Screen, Sheet, Span, Tone, Update,
    View,
};
pub use crate::terminal::Terminal;

/// What the process left behind: every session it held, and how each one stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// One entry per held session, in roster order.
    pub left: Vec<Left>,
}

/// One session the process held, and what it said.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Left {
    pub session: SessionID,
    /// The ending it reached, or `None` when the human left it open and resumable.
    pub ending: Option<Ending>,
    /// The whole conversation as lines, so a shell keeps it after the terminal is handed back.
    pub lines: Vec<Line>,
}

/// What the conversation could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The terminal refused a read or a draw.
    #[error("the terminal failed")]
    Terminal(#[source] std::io::Error),
    /// Standard input or output is not a terminal, so there is no screen to draw on.
    #[error("this is not a terminal; use 'mokmokd agent run' for a run that writes no screen")]
    NotATerminal,
    /// The working directory could not be read, so a bare start has no checkout to offer.
    #[error("the working directory could not be read")]
    Directory(#[source] std::io::Error),
    /// `--session` is not a UUID.
    #[error("{0} is not a session UUID")]
    InvalidSession(String),
    /// The log holds a history for the session and no task in it.
    #[error(
        "the session {} recorded no task, and a session runs under the task its log holds; read it \
         with 'mokmokd status --session {}'",
        .0.as_uuid(),
        .0.as_uuid()
    )]
    Untasked(SessionID),
    /// The bus could not be reached, or stopped answering.
    #[error("the bus failed: {0}")]
    Bus(#[from] mokmok_event_bus::Error),
    /// A payload was not the message this client reads.
    #[error("a bus payload could not be read: {0}")]
    BusPayload(#[from] mokmok_event_bus::PayloadError),
    /// The daemon refused the command at its boundary.
    #[error("{0}")]
    Refused(#[from] mokmok_session_wire::Refusal),
    /// The agent loop failed: the journal, the provider, or the tools.
    #[error("{0}")]
    Agent(#[from] mokmok_agent_runtime::Error),
    /// The recorded history is not one this build can fold.
    #[error("the session history is not one this build can fold: {0}")]
    Fold(#[from] mokmok_agent_runtime::Refusal),
    /// The bus thread stopped answering, or never started.
    #[error("the bus thread stopped; the daemon still holds the session")]
    BusThread,
}

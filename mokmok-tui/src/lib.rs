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

mod core;
mod port;
mod session;
mod terminal;
mod worker;

use mokmok_protocol::SessionID;
use mokmok_state_machine::Ending;

pub use crate::core::run;
pub use crate::port::{
    Engine, Event, Key, Line, Listing, Roster, Row, Screen, Sheet, Update, View,
};
pub use crate::session::{Session, Sessions};
pub use crate::terminal::Terminal;
pub use crate::worker::Worker;

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
    /// The log holds a history for the session and no task in it.
    #[error(
        "the session {} recorded no task, and a session runs under the task its log holds; read it \
         with 'mokmokd status --session {}'",
        .0.as_uuid(),
        .0.as_uuid()
    )]
    Untasked(SessionID),
    /// The configuration was refused: a grant, a program, a credential, a script.
    #[error("{0}")]
    Config(#[from] mokmok_kernel::ConfigError),
    /// The kernel could not open the log, the artifact store, the sandbox, or the provider.
    #[error("{0}")]
    Kernel(#[from] mokmok_kernel::Error),
    /// The agent loop failed: the journal, the provider, or the tools.
    #[error("{0}")]
    Agent(#[from] mokmok_agent_runtime::Error),
    /// The thread a session stepped on stopped answering.
    #[error("the session worker stopped; its log holds everything it committed")]
    Worker,
    /// The thread a session would have stepped on could not be started.
    #[error("the session worker could not start")]
    WorkerStart(#[source] std::io::Error),
}

//! A conversation with the constrained agent, drawn on a terminal.
//!
//! The screen is a pure function of the durable [`Transcript`]: the crate holds only the line being
//! typed and how far back the transcript is scrolled, and everything else the screen shows and the
//! loop does next comes from the fold. [`run`] drives the same kernel loop `mokmokd agent run`
//! drives, one bounded step at a time.
//!
//! [`Transcript`]: mokmok_agent_runtime::Transcript

mod core;
mod port;
mod session;
mod terminal;

use mokmok_state_machine::Ending;

pub use crate::core::run;
pub use crate::port::{Engine, Event, Key, Line, Screen, View};
pub use crate::session::Session;
pub use crate::terminal::Terminal;

/// How a conversation ended, and what it said.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// The ending the session reached, or `None` when the human left it open and resumable.
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
    /// The configuration was refused: a grant, a program, a credential, a script.
    #[error("{0}")]
    Config(#[from] mokmok_kernel::ConfigError),
    /// The kernel could not open the log, the artifact store, the sandbox, or the provider.
    #[error("{0}")]
    Kernel(#[from] mokmok_kernel::Error),
    /// The agent loop failed: the journal, the provider, or the tools.
    #[error("{0}")]
    Agent(#[from] mokmok_agent_runtime::Error),
}

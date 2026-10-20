//! Assembles the mokmok services and coordinates their lifecycle and recovery.
//!
//! The kernel is the composition root. It resolves the command line into one [`Config`], opens the
//! event log, runs recovery before it does new work, and holds the one serialized path that appends
//! to that log. No crate below it depends on it, and no policy of a lower crate lives here: the
//! kernel decides nothing about authority by itself, it carries a decision to the layer that
//! enforces it.
//!
//! What one run does is [`submit`]: commit the intent, ask, execute under confinement, store what
//! came out, and commit the settlement. What it costs to fail in the middle is [`reconcile`]'s
//! question, and its answer is the explicit unknown outcome rather than a guess.

pub mod agent;
mod authority;
mod cancel;
mod clock;
mod config;
mod db;
mod editor;
mod error;
mod execution;
mod host;
mod index;
mod notify;
mod provider;
mod recovery;
mod roster;
mod serve;
mod skill;
mod slice;

pub use authority::{AGENT, Authority};
pub use clock::{Clock, FixedClock, SystemClock};
pub use config::{
    Agent, Args, Command, Config, ConfigError, CrashPoint, Global, ProcessEnvironment,
    ProviderKind, ProviderLayer, ProviderSettings, Run, Submit, config_path, relative_path,
    uuid_of,
};
pub use db::{Db, Entry, Log, ResultRecord, TurnRecord};
pub use editor::Editor;
pub use error::Error;
pub use execution::{Blob, ExecutionResult, Plan, Ran, Refused};
pub use roster::{Summary, sessions};
pub use serve::{Relay, serve};
pub use slice::{Notice, Report, Submitted, mint_session, outcome_of, reconcile, status, submit};

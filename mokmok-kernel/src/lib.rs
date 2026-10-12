//! Assembles the mokmok services and coordinates their lifecycle and recovery.
//!
//! The kernel is the composition root. It wires adapters to ports, runs recovery before the
//! services that depend on it become ready, and holds the one serialized path that appends to the
//! event log. No crate below it depends on it.
//!
//! [`Config`] is the whole of what a run is configured by, resolved at the command line, which is
//! a system boundary. Everything under it takes a `&Config` and trusts it.

mod config;
mod db;
mod error;

pub use config::{Args, Command, Config, ConfigError, CrashPoint, Submit, uuid_of};
pub use db::Db;
pub use error::Error;

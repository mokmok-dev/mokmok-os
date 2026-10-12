//! Shared message contracts for mokmok OS: typed identifiers, the [`Envelope`] a message travels
//! in, and the [`SchemaVersion`] policy that decides whether a reader may read a producer.
//!
//! The crate is `no_std` with `alloc` and owns no transport, storage, or clock, so a rule over
//! instants or identities is reproducible in a test and on replay.
//!
//! - **Decoding validates.** [`Envelope::from_json`] parses with `serde` and then re-checks the
//!   schema version, the [`Source`], the [`MediaType`], and the payload, so bytes from a producer
//!   cannot reach a state the constructors refuse.
//! - **The version policy has one home.** [`SchemaVersion::compatibility`] decides that a producer
//!   at a newer minor than the reader, or at any other major, is [`Compatibility::Unsupported`],
//!   and [`Envelope::from_json`] answers [`ProtocolError::UnsupportedVersion`] with both versions
//!   instead of decoding it.

#![no_std]

extern crate alloc;

mod envelope;
mod error;
mod id;
mod message;
mod moment;
mod version;

pub use envelope::{Data, Envelope, MediaType, Source, Subject, TraceContext};
pub use error::ProtocolError;
pub use id::*;
pub use message::{Command, MessageKind};
pub use moment::Moment;
pub use version::{Compatibility, SchemaVersion};

//! A bounded append-only log per topic, served over WebSocket on a Unix socket.
//!
//! Publishers append opaque JSON events; subscribers read them from a position they can save and
//! resume from, and a subscriber that fell behind is told what it missed rather than dropped.

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod config;
mod event;
mod log;
mod payload;
mod topic;

pub use config::Retention;
pub use event::{
    Batch, Cursor, Event, Lapsed, LogEpoch, Position, Published, Refusal, Seq, Window,
};
pub use payload::{Payload, PayloadError};
pub use topic::{InvalidTopic, Topic};

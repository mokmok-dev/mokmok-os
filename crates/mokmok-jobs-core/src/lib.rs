//! Pure domain and ports for event-sourced durable jobs.
//!
//! This crate holds no I/O, no threads, and no clock. It decides, evolves, and
//! replays; the adapters do the rest. The envelope and the generic store ports come
//! from [`mokmok_events`] and are re-exported here so existing callers keep one path.

mod job;
mod publisher;
mod stream;

pub use job::{JobCommand, JobError, JobEvent, JobState, decide, evolve};
pub use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, SOURCE, SequenceType, SpecVersion,
    StoreError, StreamKey, Version, check_append, validate_history,
};
pub use publisher::EventPublisher;
pub use stream::JobId;

//! Pure domain and ports for event-sourced durable jobs.
//!
//! This crate holds no I/O, no threads, and no clock. It decides, evolves, and
//! replays; the adapters do the rest.

mod cloud_event;
mod job;
mod port;
mod stream;

pub use cloud_event::{CloudEvent, EventType, SOURCE, SequenceType, SpecVersion};
pub use job::{JobCommand, JobError, JobEvent, JobState, decide, evolve};
pub use port::{EventPublisher, EventStore, StoreError, StreamKey, check_append};
pub use stream::{JobId, Version};

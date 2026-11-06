//! Pure domain and ports for event-sourced durable jobs.
//!
//! This crate holds no I/O, no threads, and no clock. It decides, evolves, and
//! replays; the adapters do the rest.

mod job;
mod port;
mod stream;

pub use job::{JobCommand, JobError, JobEvent, JobState, decide, evolve};
pub use port::{Envelope, EventPublisher, EventStore, StoreError};
pub use stream::{JobId, Version};

//! Actor runtime and public API for event-sourced durable jobs.
//!
//! One actor per job owns that job's decision loop, so commands for the same job
//! serialize while different jobs proceed independently. A system built with
//! [`JobSystem::with_idle_timeout`] offloads an actor that has gone idle: its thread
//! exits, and the next command replays the log into a fresh actor.

mod system;

pub use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    StoreError, StreamKey, Version, decide, evolve,
};
pub use system::{Appended, Error, JobSystem};

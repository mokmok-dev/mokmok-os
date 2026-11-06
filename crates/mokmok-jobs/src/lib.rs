//! Actor runtime and public API for event-sourced durable jobs.
//!
//! One actor per job owns that job's decision loop, so commands for the same job
//! serialize while different jobs proceed independently. An actor that has gone idle
//! retires after 60 seconds by default: its thread exits, and the next command replays
//! the log into a fresh actor. [`JobSystem::with_idle_timeout`] changes the timeout and
//! [`JobSystem::without_idle_timeout`] opts out of retirement altogether.

mod system;

pub use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    StoreError, StreamKey, Version, decide, evolve,
};
pub use system::{Appended, DEFAULT_IDLE_TIMEOUT, Error, JobSystem};

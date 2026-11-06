//! Actor runtime and public API for event-sourced durable jobs.
//!
//! One actor per job owns that job's decision loop, so commands for the same job
//! serialize while different jobs proceed independently. Each live job keeps one
//! thread until the system drops; idle actors are not retired.

mod system;

pub use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    StoreError, Version, decide, evolve,
};
pub use system::{Appended, Error, JobSystem};

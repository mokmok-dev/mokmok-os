//! Actor runtime and public API for event-sourced durable jobs.
//!
//! One actor per job owns that job's decision loop, so commands for the same job
//! serialize while different jobs proceed independently. An actor that has gone idle
//! retires after 60 seconds by default: its thread exits, and the next command replays
//! the log into a fresh actor. [`JobSystem::with_idle_timeout`] changes the timeout and
//! [`JobSystem::without_idle_timeout`] opts out of retirement altogether.
//!
//! Job ids are `UUIDv7`. [`new_job_id`] generates them.

mod system;

pub use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobCommand, JobError, JobEvent, JobId, JobState,
    StoreError, StreamKey, Version, decide, evolve,
};
pub use system::{Appended, DEFAULT_IDLE_TIMEOUT, Error, JobSystem};

/// Generates the id for one new job.
///
/// Generate one per job and reuse it on retries: the id is the idempotency key.
/// A business key belongs in the submitted input.
#[must_use]
pub fn new_job_id() -> JobId {
    JobId::from_uuid(uuid::Uuid::now_v7())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_job_id_is_a_uuid_v7() {
        let first = new_job_id();
        let parsed = uuid::Uuid::parse_str(first.as_str()).expect("id is a uuid");
        assert_eq!(parsed.get_version_num(), 7);
        assert_ne!(first, new_job_id());
    }
}

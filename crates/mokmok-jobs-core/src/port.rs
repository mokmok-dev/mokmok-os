//! The two ports the outside world implements for a job system.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::job::JobEvent;
use crate::stream::{JobId, Version};

/// One event with the stream position it was committed at.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub job_id: JobId,
    pub version: Version,
    pub event: JobEvent,
}

/// A durable, append-only log of one event stream per job.
pub trait EventStore: Send + Sync {
    /// `JobSystem` calls this while it holds the job's replay lock, so an
    /// implementation must not send a command back to that same job.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::Corrupt`] when the log cannot be
    /// read or holds a record the store cannot trust.
    fn load(
        &self,
        id: &JobId,
    ) -> Result<Vec<Envelope>, StoreError>;

    /// Commits the whole slice or nothing, returning the last committed version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Conflict`] when the stream's version is not `expected`.
    fn append(
        &self,
        id: &JobId,
        expected: Version,
        events: &[JobEvent],
    ) -> Result<Version, StoreError>;
}

/// The fan-out called after a successful append.
///
/// Delivery is at-least-once from the log, and an implementation must not panic.
/// The committed job's actor blocks in this call, so an implementation must not
/// send a command back to that same job.
pub trait EventPublisher: Send + Sync {
    fn publish(
        &self,
        envelopes: &[Envelope],
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Conflict { expected: Version, actual: Version },
    Io(String),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                formatter,
                "append expected version {} but the stream is at {}",
                expected.get(),
                actual.get()
            ),
            Self::Io(message) => write!(formatter, "store i/o: {message}"),
            Self::Corrupt(message) => write!(formatter, "store is corrupt: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_store_error_has_a_message() {
        let cases = [
            (
                StoreError::Conflict {
                    expected: Version::new(1),
                    actual: Version::new(3),
                },
                "expected version 1",
            ),
            (StoreError::Io("disk".to_owned()), "disk"),
            (StoreError::Corrupt("line".to_owned()), "line"),
        ];
        for (error, expected) in cases {
            assert!(error.to_string().contains(expected));
        }
    }
}

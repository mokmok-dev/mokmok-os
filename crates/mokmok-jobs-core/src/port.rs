//! The ports the outside world implements for a job system.

use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::job::JobEvent;
use crate::stream::{JobId, Version};

/// A stable identity for one stored event stream.
///
/// [`as_str`](Self::as_str) must be a non-empty, single path component of
/// characters that are safe on every supported filesystem. [`JobId`] returns a
/// canonical lowercase UUID, so an adapter uses it as the file stem without
/// escaping.
pub trait StreamKey: Clone + Eq + Hash + Send + Sync + 'static {
    fn as_str(&self) -> &str;
}

/// One event with the stream position it was committed at.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<K, E> {
    pub stream: K,
    pub version: Version,
    pub event: E,
}

/// A durable, append-only log of one event stream per key.
pub trait EventStore<K: StreamKey, E>: Send + Sync {
    /// `JobSystem` calls this while it holds the job's replay lock, so an
    /// implementation must not send a command back to that same job.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::Corrupt`] when the log cannot be
    /// read or holds a record the store cannot trust.
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<Envelope<K, E>>, StoreError>;

    /// Commits the whole slice or nothing, returning the last committed version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Conflict`] when the stream's version is not `expected`.
    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[E],
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
        envelopes: &[Envelope<JobId, JobEvent>],
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

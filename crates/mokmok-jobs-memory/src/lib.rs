//! In-memory platform adapter for `mokmok-jobs`: a volatile event log and a
//! recording publisher for tests and single-process runs.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mokmok_jobs_core::{
    Envelope, EventPublisher, EventStore, JobEvent, JobId, StoreError, Version,
};

#[derive(Debug, Default)]
pub struct MemoryEventStore {
    streams: Mutex<HashMap<JobId, Vec<Envelope>>>,
}

impl MemoryEventStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventStore for MemoryEventStore {
    fn load(
        &self,
        id: &JobId,
    ) -> Result<Vec<Envelope>, StoreError> {
        let streams = self.streams.lock().map_err(|_| poisoned())?;
        Ok(streams.get(id).cloned().unwrap_or_default())
    }

    fn append(
        &self,
        id: &JobId,
        expected: Version,
        events: &[JobEvent],
    ) -> Result<Version, StoreError> {
        let mut streams = self.streams.lock().map_err(|_| poisoned())?;
        let log = streams.entry(id.clone()).or_default();
        let length = u64::try_from(log.len()).map_err(|error| StoreError::Io(error.to_string()))?;
        let actual = Version::new(length);
        if actual != expected {
            return Err(StoreError::Conflict { expected, actual });
        }
        let mut version = actual;
        for event in events {
            version = version.next();
            log.push(Envelope {
                job_id: id.clone(),
                version,
                event: event.clone(),
            });
        }
        drop(streams);
        Ok(version)
    }
}

#[derive(Debug, Default)]
pub struct RecordingPublisher {
    published: Mutex<Vec<Envelope>>,
}

impl RecordingPublisher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn published(&self) -> Vec<Envelope> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[allow(clippy::len_without_is_empty)] // Nothing asks a recording whether it is empty.
    #[must_use]
    pub fn len(&self) -> usize {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl EventPublisher for RecordingPublisher {
    fn publish(
        &self,
        envelopes: &[Envelope],
    ) {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(envelopes);
    }
}

fn poisoned() -> StoreError {
    StoreError::Io("event store lock poisoned".to_owned())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn id() -> JobId {
        JobId::parse("memory").expect("literal is valid")
    }

    fn submitted() -> JobEvent {
        JobEvent::Submitted {
            input: b"in".to_vec(),
            max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
        }
    }

    #[test]
    fn append_commits_a_contiguous_stream() {
        let store = MemoryEventStore::new();
        let id = id();
        let last = store
            .append(&id, Version::ZERO, &[submitted()])
            .expect("first append fits");
        assert_eq!(last, Version::new(1));
        let last = store
            .append(&id, Version::new(1), &[JobEvent::Started { attempt: 1 }])
            .expect("second append fits");
        assert_eq!(last, Version::new(2));
        let events = store.load(&id).expect("stream reads back");
        let versions: Vec<u64> = events.iter().map(|event| event.version.get()).collect();
        assert_eq!(versions, vec![1, 2]);
    }

    #[test]
    fn append_rejects_a_stale_expected_version() {
        let store = MemoryEventStore::new();
        let id = id();
        store
            .append(&id, Version::ZERO, &[submitted()])
            .expect("first append fits");
        let error = store
            .append(&id, Version::ZERO, &[JobEvent::Started { attempt: 1 }])
            .expect_err("stale append is refused");
        assert!(matches!(
            error,
            StoreError::Conflict {
                expected,
                actual,
            } if expected == Version::ZERO && actual == Version::new(1)
        ));
        assert_eq!(store.load(&id).expect("stream reads back").len(), 1);
    }

    #[test]
    fn publisher_records_every_event_in_order() {
        let publisher = RecordingPublisher::new();
        let id = id();
        let first = Envelope {
            job_id: id.clone(),
            version: Version::new(1),
            event: submitted(),
        };
        let second = Envelope {
            job_id: id,
            version: Version::new(2),
            event: JobEvent::Started { attempt: 1 },
        };
        publisher.publish(std::slice::from_ref(&first));
        publisher.publish(std::slice::from_ref(&second));
        assert_eq!(publisher.published(), vec![first, second]);
        assert_eq!(publisher.len(), 2);
    }
}

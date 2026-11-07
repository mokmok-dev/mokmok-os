//! In-memory platform adapter for `mokmok-jobs`: a volatile event log and a
//! recording publisher for tests and single-process runs.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mokmok_jobs_core::{
    CloudEvent, EventPublisher, EventStore, JobEvent, StoreError, StreamKey, Version, check_append,
};

#[derive(Debug)]
pub struct MemoryEventStore<K, E> {
    streams: Mutex<HashMap<K, Vec<CloudEvent<E>>>>,
}

impl<K, E> Default for MemoryEventStore<K, E> {
    fn default() -> Self {
        Self {
            streams: Mutex::new(HashMap::new()),
        }
    }
}

impl<K, E> MemoryEventStore<K, E> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K: StreamKey, E: Clone + Send + Sync> EventStore<K, E> for MemoryEventStore<K, E> {
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<CloudEvent<E>>, StoreError> {
        let streams = self.streams.lock().map_err(|_| poisoned())?;
        Ok(streams.get(key).cloned().unwrap_or_default())
    }

    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[CloudEvent<E>],
    ) -> Result<Version, StoreError> {
        let mut streams = self.streams.lock().map_err(|_| poisoned())?;
        let log = streams.entry(key.clone()).or_default();
        let length = u64::try_from(log.len()).map_err(|error| StoreError::Io(error.to_string()))?;
        let actual = Version::new(length);
        if actual != expected {
            return Err(StoreError::Conflict { expected, actual });
        }
        let last = check_append(key.as_str(), expected, events)?;
        log.extend_from_slice(events);
        drop(streams);
        Ok(last)
    }
}

#[derive(Debug, Default)]
pub struct RecordingPublisher {
    published: Mutex<Vec<CloudEvent<JobEvent>>>,
}

impl RecordingPublisher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn published(&self) -> Vec<CloudEvent<JobEvent>> {
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
        events: &[CloudEvent<JobEvent>],
    ) {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(events);
    }
}

fn poisoned() -> StoreError {
    StoreError::Io("event store lock poisoned".to_owned())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use mokmok_jobs_core::JobId;

    use super::*;

    fn id() -> JobId {
        JobId::from_u128(1)
    }

    fn submitted() -> JobEvent {
        JobEvent::Submitted {
            input: b"in".to_vec(),
            max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
        }
    }

    fn at(
        id: &JobId,
        version: u64,
        event: JobEvent,
    ) -> CloudEvent<JobEvent> {
        CloudEvent::new(id.as_str(), Version::new(version), event)
    }

    #[test]
    fn append_commits_a_contiguous_stream() {
        let store = MemoryEventStore::new();
        let id = id();
        let last = store
            .append(&id, Version::ZERO, &[at(&id, 1, submitted())])
            .expect("first append fits");
        assert_eq!(last, Version::new(1));
        let last = store
            .append(
                &id,
                Version::new(1),
                &[at(&id, 2, JobEvent::Started { attempt: 1 })],
            )
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
            .append(&id, Version::ZERO, &[at(&id, 1, submitted())])
            .expect("first append fits");
        let error = store
            .append(
                &id,
                Version::ZERO,
                &[at(&id, 1, JobEvent::Started { attempt: 1 })],
            )
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
    fn append_rejects_a_record_from_another_stream() {
        let store = MemoryEventStore::new();
        let id = id();
        let other = JobId::from_u128(2);
        let error = store
            .append(&id, Version::ZERO, &[at(&other, 1, submitted())])
            .expect_err("a record from another stream is refused");
        assert!(matches!(error, StoreError::Invalid(_)));
        assert_eq!(store.load(&id).expect("stream reads back").len(), 0);
    }

    #[test]
    fn publisher_records_every_event_in_order() {
        let publisher = RecordingPublisher::new();
        let id = id();
        let first = at(&id, 1, submitted());
        let second = at(&id, 2, JobEvent::Started { attempt: 1 });
        publisher.publish(std::slice::from_ref(&first));
        publisher.publish(std::slice::from_ref(&second));
        assert_eq!(publisher.published(), vec![first, second]);
        assert_eq!(publisher.len(), 2);
    }
}

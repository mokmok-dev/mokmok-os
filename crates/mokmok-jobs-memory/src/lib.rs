//! In-memory platform adapter for `mokmok-jobs`: a volatile event log and a
//! recording publisher for tests and single-process runs.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, StoreError, StreamKey, Version, check_append,
};
use mokmok_jobs_core::{EventPublisher, JobEvent};

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
        let actual = match streams.get(key) {
            Some(log) => Version::new(
                u64::try_from(log.len()).map_err(|error| StoreError::Io(error.to_string()))?,
            ),
            None => Version::ZERO,
        };
        if actual != expected {
            return Err(StoreError::Conflict { expected, actual });
        }
        if events.is_empty() {
            return Ok(actual);
        }
        let last = check_append(key.as_str(), expected, events)?;
        streams
            .entry(key.clone())
            .or_default()
            .extend_from_slice(events);
        drop(streams);
        Ok(last)
    }
}

impl<K: StreamKey, E: Clone + Send + Sync> EnumerableStore<K, E> for MemoryEventStore<K, E> {
    fn keys(&self) -> Result<Vec<K>, StoreError> {
        let mut keys: Vec<K> = {
            let streams = self.streams.lock().map_err(|_| poisoned())?;
            streams.keys().cloned().collect()
        };
        keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(keys)
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

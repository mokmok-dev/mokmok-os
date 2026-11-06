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

//! The shared store contract, run against the `JSONL` adapter.
//!
//! The adapter's own tests stay in `src/lib.rs`. This file holds only what the shared
//! suite asks of every backend, so a contract change lands in one crate.

#![allow(clippy::expect_used)]

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use mokmok_events::{CloudEvent, EventStore, StoreError, Version, object_name};
use mokmok_events_conformance::{
    Storage, StoreFactory, check, check_multiple_handles, check_storage,
};
use mokmok_jobs_core::{JobEvent, JobId};
use mokmok_jobs_jsonl::JsonlEventStore;
use tempfile::TempDir;

struct Directory {
    root: PathBuf,
    _temporary: TempDir,
}

impl Directory {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("a temporary directory");
        let root = temporary.path().join("jobs");
        Self {
            root,
            _temporary: temporary,
        }
    }

    fn stream(&self) -> PathBuf {
        self.root.clone()
    }

    fn file(
        &self,
        key: &JobId,
    ) -> PathBuf {
        self.root.join(object_name(key.as_str()))
    }
}

impl StoreFactory<JobId, JobEvent> for Directory {
    type Store = JsonlEventStore<JobId, JobEvent>;

    fn open(&self) -> Self::Store {
        JsonlEventStore::open(self.stream()).expect("the store opens")
    }

    fn key(
        &self,
        seed: u64,
    ) -> JobId {
        JobId::from_u128(u128::from(seed))
    }
}

impl Storage<JobId, JobEvent> for Directory {
    fn plant(
        &self,
        key: &JobId,
        committed: &[CloudEvent<JobEvent>],
        torn_tail: &[u8],
    ) {
        let mut bytes = Vec::new();
        mokmok_events::encode_records(committed, &mut bytes).expect("the records encode");
        bytes.extend_from_slice(torn_tail);
        write(&self.file(key), &bytes);
    }

    fn plant_stray(
        &self,
        name: &str,
        bytes: &[u8],
    ) {
        write(&self.root.join(name), bytes);
    }
}

fn write(
    path: &Path,
    bytes: &[u8],
) {
    std::fs::create_dir_all(path.parent().expect("a stream sits inside the root"))
        .expect("the root exists");
    std::fs::write(path, bytes).expect("the fixture writes");
}

fn submitted() -> JobEvent {
    JobEvent::Submitted {
        input: b"in".to_vec(),
        max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
    }
}

const fn started() -> JobEvent {
    JobEvent::Started { attempt: 1 }
}

#[test]
fn the_store_meets_the_event_store_contract() {
    check(&Directory::new(), submitted(), started());
}

#[test]
fn the_store_reads_a_torn_tail_and_skips_a_stray_entry() {
    check_storage(&Directory::new(), submitted(), started());
}

#[test]
#[ignore = "MOK-243: a second handle over the same root accepts a stale version"]
fn independent_handles_reject_stale_appends() {
    check_multiple_handles(&Directory::new(), submitted(), started());
}

/// This adapter's stale handle reports success and writes version 2 a second time,
/// which leaves the stream unreadable: the next `load` refuses the duplicate position.
#[test]
fn the_stale_handle_writes_a_duplicate_version_that_makes_the_stream_unreadable() {
    let directory = Directory::new();
    let left = directory.open();
    let right = directory.open();
    let key = directory.key(1);
    let one = CloudEvent::new(key.as_str(), Version::new(1), submitted());
    let two = CloudEvent::new(key.as_str(), Version::new(2), started());

    left.append(&key, Version::ZERO, &[one])
        .expect("the left handle commits");
    right
        .append(&key, Version::new(1), std::slice::from_ref(&two))
        .expect("the right handle commits");

    assert_eq!(
        left.append(&key, Version::new(1), &[two]),
        Ok(Version::new(2)),
        "the cached handle reports success and duplicates the version"
    );
    let error = right
        .load(&key)
        .expect_err("the duplicated position is not a stream a writer produces");
    assert!(
        matches!(&error, StoreError::Corrupt(message) if message.contains("expected version 3")),
        "{error:?}"
    );
}

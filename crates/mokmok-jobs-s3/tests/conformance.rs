//! The shared store contract, run against the `S3` adapter and the fake server.
//!
//! The adapter's own tests stay in `store.rs`. This file holds only what the shared
//! suite asks of every backend, so a contract change lands in one crate.

#![allow(clippy::expect_used)]

mod common;

use std::num::NonZeroU32;

use common::FakeS3;
use mokmok_events::CloudEvent;
use mokmok_events_conformance::{
    Storage, StoreFactory, check, check_multiple_handles, check_storage,
};
use mokmok_jobs_core::{JobEvent, JobId};
use mokmok_jobs_s3::S3EventStore;

const BUCKET: &str = "mokmok";
const PREFIX: &str = "jobs";

struct Server {
    fake: FakeS3,
}

impl Server {
    fn start() -> Self {
        Self {
            fake: FakeS3::start(),
        }
    }
}

impl StoreFactory<JobId, JobEvent> for Server {
    type Store = S3EventStore<JobId, JobEvent>;

    fn open(&self) -> Self::Store {
        S3EventStore::open(self.fake.config(BUCKET, PREFIX)).expect("the store opens")
    }

    fn key(
        &self,
        seed: u64,
    ) -> JobId {
        JobId::from_u128(u128::from(seed))
    }
}

impl Storage<JobId, JobEvent> for Server {
    fn plant(
        &self,
        key: &JobId,
        committed: &[CloudEvent<JobEvent>],
        torn_tail: &[u8],
    ) {
        let mut bytes = Vec::new();
        mokmok_events::encode_records(committed, &mut bytes).expect("the records encode");
        bytes.extend_from_slice(torn_tail);
        self.fake.put_object(
            BUCKET,
            &format!("{PREFIX}/{}", mokmok_events::object_name(key.as_str())),
            &bytes,
        );
    }

    fn plant_stray(&self) {
        self.fake
            .put_object(BUCKET, &format!("{PREFIX}/notes.txt"), b"not a stream");
    }
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
    check(&Server::start(), submitted(), started());
}

#[test]
fn the_store_reads_a_torn_tail_and_skips_a_stray_entry() {
    check_storage(&Server::start(), submitted(), started());
}

/// Two handles over one bucket are two daemons on one prefix, which the adapter does
/// not support. It still must refuse a version another handle committed, because every
/// append re-reads the object and writes it back under `If-Match`.
#[test]
fn two_handles_reject_a_stale_append() {
    check_multiple_handles(&Server::start(), submitted(), started());
}

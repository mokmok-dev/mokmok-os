//! The shared store contract, run against the in-memory adapter.
//!
//! The memory store keeps events, not bytes, so it answers [`check`] and not the
//! storage tier: a torn tail and a stray entry are things a representation at rest can
//! have, and this adapter has none. The multi-handle tier does not apply either, because
//! [`MemoryEventStore::new`] makes a fresh map per handle, so a second handle is a second
//! store. The actor systems share one instance behind an `Arc`, which is the single
//! handle [`check`] already drives.

#![allow(clippy::expect_used)]

use std::num::NonZeroU32;

use mokmok_events_conformance::{StoreFactory, check};
use mokmok_jobs_core::{JobEvent, JobId};
use mokmok_jobs_memory::MemoryEventStore;

struct Memory;

impl StoreFactory<JobId, JobEvent> for Memory {
    type Store = MemoryEventStore<JobId, JobEvent>;

    fn open(&self) -> Self::Store {
        MemoryEventStore::new()
    }

    fn key(
        &self,
        seed: u64,
    ) -> JobId {
        JobId::from_u128(u128::from(seed))
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
    check(&Memory, submitted(), started());
}

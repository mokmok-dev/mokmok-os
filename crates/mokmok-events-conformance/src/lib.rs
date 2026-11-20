//! The executable [`EventStore`] contract every adapter must meet.
//!
//! An adapter keeps its own tests for a file, a bucket, or a cache. This crate is the
//! overlap, and the one place a store rule is written down. A backend supplies a
//! [`StoreFactory`] and its test calls the tier its storage supports, so a rule changes
//! once and every backend is held to it.
//!
//! Three tiers, because the port makes three tiers of promise:
//!
//! - [`check`] holds any store that implements `EventStore` and `EnumerableStore`.
//! - [`check_storage`] adds the clauses that only mean something for an adapter that
//!   keeps bytes: a torn tail, and a stray entry beside the streams. It needs a
//!   [`Storage`].
//! - [`check_multiple_handles`] adds the clause for an adapter where a second handle over
//!   the same storage is possible.
//!
//! Adding a backend means implementing [`StoreFactory`] in that crate's tests and
//! calling the tiers its storage supports. The suite never learns the backend's name.

#![allow(clippy::expect_used, clippy::panic)]

use std::fmt::Debug;

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, StoreError, StreamKey, Version,
};

/// Opens handles over one backend's storage.
///
/// A fixture owns whatever the backend needs to exist, such as a temporary directory or
/// a fake server, and lives at least as long as the handles it opens.
pub trait StoreFactory<K: StreamKey, E> {
    type Store: EnumerableStore<K, E>;

    /// A backend that calls [`check_multiple_handles`] must return handles over the same
    /// stored streams. One that cannot is a single-handle backend and skips that tier.
    fn open(&self) -> Self::Store;

    /// A canonical key for `seed`, distinct for distinct seeds.
    ///
    /// The suite picks the seeds it needs and never builds a key itself, so a backend
    /// whose keys are typed or branded keeps that rule in one place.
    fn key(
        &self,
        seed: u64,
    ) -> K;
}

/// A [`StoreFactory`] whose streams are bytes at rest.
///
/// This is what [`check_storage`] needs beyond [`check`]: the ability to leave a stream
/// as a writer that died would have left it, and to place entries beside the streams.
pub trait Storage<K: StreamKey, E>: StoreFactory<K, E> {
    /// Stores `planted`, then `torn_tail`, as the bytes of `key`'s stream.
    ///
    /// That is what a writer leaves when it dies mid-record: whole records up to the
    /// last newline, then a partial one. `torn_tail` is not a record any writer
    /// committed.
    fn plant(
        &self,
        key: &K,
        planted: &[CloudEvent<E>],
        torn_tail: &[u8],
    );

    /// Writes an entry beside the streams whose name is `name`.
    ///
    /// The suite passes two names that no stream can occupy: one that is not a stream
    /// object at all, and one that carries the stream suffix over a stem that is not a
    /// key. Both must stay out of [`EnumerableStore::keys`].
    fn plant_stray(
        &self,
        name: &str,
        bytes: &[u8],
    );
}

/// Runs every store check that needs no access to the stored bytes.
///
/// The fixture's storage must be empty. Seeds `1` and `2` name the two streams it uses,
/// and an event must be one its backend's records can carry.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check<F, K, E>(
    fixture: &F,
    first: E,
    second: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq + Send + Sync,
    F: StoreFactory<K, E>,
{
    let alpha = fixture.key(1);
    let beta = fixture.key(2);
    assert_ne!(
        alpha, beta,
        "the two probe keys must name different streams"
    );
    let store = fixture.open();
    assert_eq!(
        store.keys().expect("a fresh store lists"),
        Vec::<K>::new(),
        "a fresh store lists no streams"
    );

    a_missing_stream_loads_empty(&store, &alpha);
    an_empty_append_does_not_create_a_stream(&store, &alpha);
    a_contiguous_append_round_trips(&store, &alpha, first.clone(), second.clone());
    a_stale_append_is_a_conflict(&store, &alpha, first.clone());
    a_record_for_another_stream_is_refused(&store, &beta, &alpha, first.clone());
    a_skipped_version_is_refused(&store, &beta, second);
    two_streams_are_independent(&store, &alpha, &beta, first);
    keys_lists_every_stream_in_key_order(&store, &alpha, &beta);
}

/// Runs [`check`] and the clauses a backend that keeps bytes must also meet.
///
/// Uses seed `3` for the stream it plants.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check_storage<F, K, E>(
    fixture: &F,
    first: E,
    second: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq + Send + Sync,
    F: Storage<K, E>,
{
    check(fixture, first.clone(), second.clone());
    let store = fixture.open();
    let key = fixture.key(3);
    let one = record(&key, 1, first);
    let two = record(&key, 2, second);

    fixture.plant(&key, std::slice::from_ref(&one), b"{\"specversion\":");
    assert_eq!(
        store
            .load(&key)
            .unwrap_or_else(|error| panic!("a torn tail must be read as records only: {error}")),
        vec![one.clone()],
        "a torn tail is not a record"
    );
    let committed = store
        .append(&key, Version::new(1), std::slice::from_ref(&two))
        .unwrap_or_else(|error| panic!("the next append must drop the torn tail: {error}"));
    assert_eq!(committed, Version::new(2), "the append lands past the tail");
    assert_eq!(
        store.load(&key).unwrap_or_else(|error| panic!(
            "the torn tail must be gone, not written over: {error}"
        )),
        vec![one, two],
        "the torn tail is gone after the append"
    );

    let listed = store.keys().expect("the store lists before a stray entry");
    fixture.plant_stray("notes.txt", b"not a stream");
    fixture.plant_stray("not-a-key.jsonl", b"not a stream either");
    assert_eq!(
        store.keys().expect("the store lists after a stray entry"),
        listed,
        "a stray entry must not be listed"
    );
}

/// Runs [`check`] and the clause for two handles over the same storage.
///
/// A backend whose storage cannot be opened twice does not call this. Uses seed `4`.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check_multiple_handles<F, K, E>(
    fixture: &F,
    first: E,
    second: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq + Send + Sync,
    F: StoreFactory<K, E>,
{
    check(fixture, first.clone(), second.clone());
    let key = fixture.key(4);
    let one = record(&key, 1, first);
    let two = record(&key, 2, second);
    let left = fixture.open();
    let right = fixture.open();

    assert_eq!(
        left.append(&key, Version::ZERO, std::slice::from_ref(&one))
            .expect("the first handle commits"),
        Version::new(1)
    );
    assert_eq!(
        right
            .append(&key, Version::new(1), std::slice::from_ref(&two))
            .expect("the second handle commits"),
        Version::new(2)
    );
    let error = left
        .append(&key, Version::new(1), std::slice::from_ref(&two))
        .expect_err("a second handle must reject the stale version");
    assert!(
        matches!(
            error,
            StoreError::Conflict { expected, actual }
                if expected == Version::new(1) && actual == Version::new(2)
        ),
        "the stale append must name what the other handle stored: {error:?}"
    );
    assert_eq!(
        right.load(&key).expect("the stream reads back"),
        vec![one, two],
        "a refused append must leave the log alone"
    );
}

fn record<K: StreamKey, E: EventType>(
    key: &K,
    version: u64,
    data: E,
) -> CloudEvent<E> {
    CloudEvent::new(key.as_str(), Version::new(version), data)
}

fn a_missing_stream_loads_empty<K, E, S>(
    store: &S,
    key: &K,
) where
    K: StreamKey + Debug,
    E: Debug + PartialEq,
    S: EventStore<K, E>,
{
    let loaded = store.load(key).expect("a missing stream loads");
    assert_eq!(
        loaded,
        Vec::<CloudEvent<E>>::new(),
        "a missing stream loads empty"
    );
}

fn an_empty_append_does_not_create_a_stream<K, E, S>(
    store: &S,
    key: &K,
) where
    K: StreamKey + Debug,
    E: Debug,
    S: EventStore<K, E> + EnumerableStore<K, E>,
{
    assert_eq!(
        store
            .append(key, Version::ZERO, &[])
            .expect("an empty append at zero succeeds"),
        Version::ZERO
    );
    assert_eq!(
        store.keys().expect("keys after an empty append"),
        Vec::<K>::new(),
        "an empty append must not materialise a stream"
    );
    assert_eq!(
        store.load(key).expect("load after an empty append").len(),
        0
    );
}

fn a_contiguous_append_round_trips<K, E, S>(
    store: &S,
    key: &K,
    first: E,
    second: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq,
    S: EventStore<K, E>,
{
    let events = [record(key, 1, first), record(key, 2, second)];
    assert_eq!(
        store
            .append(key, Version::ZERO, &events)
            .expect("a contiguous append commits"),
        Version::new(2)
    );
    assert_eq!(store.load(key).expect("the stream reads back"), events);
    assert_eq!(
        store
            .append(key, Version::new(2), &[])
            .expect("an empty append at the tip succeeds"),
        Version::new(2)
    );
    assert_eq!(
        store.load(key).expect("empty append leaves the log").len(),
        2
    );
}

fn a_stale_append_is_a_conflict<K, E, S>(
    store: &S,
    key: &K,
    first: E,
) where
    K: StreamKey + Debug,
    E: EventType + Debug,
    S: EventStore<K, E>,
{
    let error = store
        .append(key, Version::ZERO, &[record(key, 1, first)])
        .expect_err("a stale append is refused");
    assert!(
        matches!(
            error,
            StoreError::Conflict {
                expected,
                actual,
            } if expected == Version::ZERO && actual == Version::new(2)
        ),
        "stale append must name the stored version: {error:?}"
    );
    assert_eq!(store.load(key).expect("the stream reads back").len(), 2);
}

fn a_record_for_another_stream_is_refused<K, E, S>(
    store: &S,
    key: &K,
    other: &K,
    first: E,
) where
    K: StreamKey + Debug,
    E: EventType + Debug,
    S: EventStore<K, E> + EnumerableStore<K, E>,
{
    let error = store
        .append(key, Version::ZERO, &[record(other, 1, first)])
        .expect_err("a record from another stream is refused");
    assert!(
        matches!(error, StoreError::Invalid(_)),
        "foreign subject must be invalid: {error:?}"
    );
    assert_eq!(
        store.load(key).expect("the refused stream loads").len(),
        0,
        "a refused append must leave the stream empty"
    );
    let keys = store.keys().expect("keys after a refused append");
    assert!(
        !keys.iter().any(|listed| listed == key),
        "a refused append must not list the unused key: {keys:?}"
    );
}

fn a_skipped_version_is_refused<K, E, S>(
    store: &S,
    key: &K,
    second: E,
) where
    K: StreamKey + Debug,
    E: EventType + Debug,
    S: EventStore<K, E>,
{
    let error = store
        .append(key, Version::ZERO, &[record(key, 2, second)])
        .expect_err("a skipped version is refused");
    assert!(
        matches!(error, StoreError::Invalid(_)),
        "a gap must be invalid: {error:?}"
    );
    assert_eq!(store.load(key).expect("the refused stream loads").len(), 0);
}

fn two_streams_are_independent<K, E, S>(
    store: &S,
    alpha: &K,
    beta: &K,
    first: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq,
    S: EventStore<K, E>,
{
    store
        .append(beta, Version::ZERO, &[record(beta, 1, first)])
        .expect("the second stream commits");
    assert_eq!(store.load(beta).expect("beta reads").len(), 1);
    assert_eq!(
        store.load(alpha).expect("alpha is unchanged").len(),
        2,
        "an append to beta must not rewrite alpha"
    );
}

fn keys_lists_every_stream_in_key_order<K, E, S>(
    store: &S,
    alpha: &K,
    beta: &K,
) where
    K: StreamKey + Debug,
    S: EnumerableStore<K, E>,
{
    let mut expected = vec![alpha.clone(), beta.clone()];
    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(
        store.keys().expect("the store lists its streams"),
        expected,
        "keys must be sorted by the key's text"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use mokmok_events::{
        OBJECT_SUFFIX, canonical_keys, check_append, complete_prefix, encode_records, parse_records,
    };
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    struct Probe(String);

    impl StreamKey for Probe {
        fn as_str(&self) -> &str {
            self.0.as_str()
        }

        fn parse_key(raw: &str) -> Result<Self, StoreError> {
            raw.starts_with("probe-")
                .then(|| Self(raw.to_owned()))
                .ok_or_else(|| StoreError::Invalid(format!("{raw:?} is not a probe key")))
        }
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct Tick(u64);

    impl EventType for Tick {
        fn event_type(&self) -> &'static str {
            "dev.mokmok.tick"
        }
    }

    /// One deliberate way to break the contract.
    ///
    /// Each variant names the clause it violates, so the negative tests form one table
    /// and a new clause gets a new variant beside its check.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Deviation {
        None,
        /// A stream that was never written reads as one record.
        GhostRecord,
        /// An empty append creates the stream.
        GhostStream,
        /// `append` ignores the expected version.
        NoVersionCheck,
        /// A conflict reports the expected version as the stored one.
        WrongConflictVersion,
        /// A record that names another stream is accepted.
        ForeignSubjectAccepted,
        /// A record that skips a position is accepted.
        GapAccepted,
        /// `keys` returns the streams in reverse order.
        UnsortedKeys,
        /// `keys` returns an entry that is not a stream.
        StrayListed,
        /// The next append writes onto the torn tail instead of dropping it.
        TornTailKept,
        /// A handle trusts its own cache over what the storage holds.
        StaleHandleAccepted,
    }

    struct Shared {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        deviation: Deviation,
    }

    struct ProbeStore {
        shared: Arc<Shared>,
        seen: Mutex<BTreeMap<Probe, Version>>,
    }

    struct ProbeFixture {
        shared: Arc<Shared>,
    }

    impl ProbeFixture {
        fn new(deviation: Deviation) -> Self {
            Self {
                shared: Arc::new(Shared {
                    objects: Mutex::new(BTreeMap::new()),
                    deviation,
                }),
            }
        }
    }

    impl StoreFactory<Probe, Tick> for ProbeFixture {
        type Store = ProbeStore;

        fn open(&self) -> Self::Store {
            ProbeStore {
                shared: Arc::clone(&self.shared),
                seen: Mutex::new(BTreeMap::new()),
            }
        }

        fn key(
            &self,
            seed: u64,
        ) -> Probe {
            Probe(format!("probe-{seed}"))
        }
    }

    impl Storage<Probe, Tick> for ProbeFixture {
        fn plant(
            &self,
            key: &Probe,
            planted: &[CloudEvent<Tick>],
            torn_tail: &[u8],
        ) {
            let mut bytes = Vec::new();
            encode_records(planted, &mut bytes).expect("the probe encodes");
            bytes.extend_from_slice(torn_tail);
            self.shared
                .objects
                .lock()
                .expect("the probe is not poisoned")
                .insert(name(key), bytes);
        }

        fn plant_stray(
            &self,
            name: &str,
            bytes: &[u8],
        ) {
            self.shared
                .objects
                .lock()
                .expect("the probe is not poisoned")
                .insert(name.to_owned(), bytes.to_vec());
        }
    }

    impl EventStore<Probe, Tick> for ProbeStore {
        fn load(
            &self,
            key: &Probe,
        ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
            let bytes = self
                .shared
                .objects
                .lock()
                .expect("the probe is not poisoned")
                .get(&name(key))
                .cloned();
            let bytes = match (bytes, self.shared.deviation) {
                (None, Deviation::GhostRecord) => {
                    return Ok(vec![record(key, 0, Tick(0))]);
                },
                (bytes, _) => bytes,
            };
            let Some(bytes) = bytes else {
                return Ok(Vec::new());
            };
            parse_records("probe", key.as_str(), complete_prefix(&bytes)).map(|(_, events)| events)
        }

        fn append(
            &self,
            key: &Probe,
            expected: Version,
            events: &[CloudEvent<Tick>],
        ) -> Result<Version, StoreError> {
            let mut objects = self
                .shared
                .objects
                .lock()
                .expect("the probe is not poisoned");
            let bytes = objects.get(&name(key)).cloned().unwrap_or_default();
            let committed = complete_prefix(&bytes).to_vec();
            let (stored, _) = parse_records::<Tick>("probe", key.as_str(), &committed)?;
            let actual = match self.shared.deviation {
                Deviation::StaleHandleAccepted => {
                    let mut seen = self.seen.lock().expect("the probe is not poisoned");
                    *seen.entry(key.clone()).or_insert(stored)
                },
                _ => stored,
            };
            if actual != expected && self.shared.deviation != Deviation::NoVersionCheck {
                let reported = if self.shared.deviation == Deviation::WrongConflictVersion {
                    expected
                } else {
                    actual
                };
                return Err(StoreError::Conflict {
                    expected,
                    actual: reported,
                });
            }
            if events.is_empty() {
                if self.shared.deviation == Deviation::GhostStream {
                    objects.entry(name(key)).or_default();
                }
                return Ok(actual);
            }
            let last = match self.shared.deviation {
                Deviation::ForeignSubjectAccepted => {
                    Version::new(expected.get() + events.len() as u64)
                },
                Deviation::GapAccepted => {
                    for event in events {
                        if event.subject != key.as_str() {
                            return Err(StoreError::Invalid("foreign subject".to_owned()));
                        }
                    }
                    Version::new(expected.get() + events.len() as u64)
                },
                _ => check_append(key.as_str(), expected, events)?,
            };
            let mut next = if self.shared.deviation == Deviation::TornTailKept {
                bytes
            } else {
                committed
            };
            encode_records(events, &mut next)?;
            objects.insert(name(key), next);
            drop(objects);
            if self.shared.deviation == Deviation::StaleHandleAccepted {
                self.seen
                    .lock()
                    .expect("the probe is not poisoned")
                    .insert(key.clone(), last);
            }
            Ok(last)
        }
    }

    impl EnumerableStore<Probe, Tick> for ProbeStore {
        fn keys(&self) -> Result<Vec<Probe>, StoreError> {
            let names: Vec<String> = self
                .shared
                .objects
                .lock()
                .expect("the probe is not poisoned")
                .keys()
                .cloned()
                .collect();
            let mut keys = match self.shared.deviation {
                // Lists every name, suffix or not, without asking whether it is a key.
                Deviation::StrayListed => names
                    .iter()
                    .map(|name| Probe(name.strip_suffix(OBJECT_SUFFIX).unwrap_or(name).to_owned()))
                    .collect(),
                _ => canonical_keys(names),
            };
            if self.shared.deviation == Deviation::UnsortedKeys {
                keys.reverse();
            }
            Ok(keys)
        }
    }

    fn name(key: &Probe) -> String {
        format!("{}{OBJECT_SUFFIX}", key.as_str())
    }

    /// Runs `run` against a store with `deviation` and requires the suite to fail at
    /// the clause named by `expected`.
    fn rejects(
        deviation: Deviation,
        expected: &str,
        run: impl FnOnce(&ProbeFixture),
    ) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&ProbeFixture::new(deviation));
        }));
        let payload = outcome.expect_err("the suite must reject this store");
        let message = payload.downcast_ref::<String>().map_or_else(
            || {
                payload
                    .downcast_ref::<&str>()
                    .copied()
                    .unwrap_or_default()
                    .to_owned()
            },
            Clone::clone,
        );
        assert!(
            message.contains(expected),
            "the {deviation:?} store must fail at {expected:?}, got {message:?}"
        );
    }

    #[test]
    fn a_faithful_store_passes_every_tier() {
        check(&ProbeFixture::new(Deviation::None), Tick(1), Tick(2));
        check_storage(&ProbeFixture::new(Deviation::None), Tick(3), Tick(4));
        check_multiple_handles(&ProbeFixture::new(Deviation::None), Tick(5), Tick(6));
    }

    #[test]
    fn a_stream_that_was_never_written_reading_as_a_record_fails_the_contract() {
        rejects(
            Deviation::GhostRecord,
            "a missing stream loads empty",
            |fixture| {
                check(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn an_empty_append_that_creates_a_stream_fails_the_contract() {
        rejects(
            Deviation::GhostStream,
            "an empty append must not materialise",
            |fixture| {
                check(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn an_append_that_skips_the_version_check_fails_the_contract() {
        rejects(
            Deviation::NoVersionCheck,
            "a stale append is refused",
            |fixture| {
                check(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn a_conflict_that_hides_the_stored_version_fails_the_contract() {
        rejects(
            Deviation::WrongConflictVersion,
            "stale append must name the stored version",
            |fixture| check(fixture, Tick(1), Tick(2)),
        );
    }

    #[test]
    fn a_foreign_subject_that_is_accepted_fails_the_contract() {
        rejects(
            Deviation::ForeignSubjectAccepted,
            "a record from another stream is refused",
            |fixture| check(fixture, Tick(1), Tick(2)),
        );
    }

    #[test]
    fn a_skipped_version_that_is_accepted_fails_the_contract() {
        rejects(
            Deviation::GapAccepted,
            "a skipped version is refused",
            |fixture| {
                check(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn keys_that_are_not_ordered_fail_the_contract() {
        rejects(Deviation::UnsortedKeys, "keys must be sorted", |fixture| {
            check(fixture, Tick(1), Tick(2));
        });
    }

    #[test]
    fn a_torn_tail_that_the_append_keeps_fails_the_contract() {
        rejects(
            Deviation::TornTailKept,
            "the torn tail must be gone",
            |fixture| {
                check_storage(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn keys_that_list_a_stray_entry_fail_the_contract() {
        rejects(
            Deviation::StrayListed,
            "a stray entry must not be listed",
            |fixture| {
                check_storage(fixture, Tick(1), Tick(2));
            },
        );
    }

    #[test]
    fn a_handle_that_trusts_its_own_cache_fails_the_contract() {
        rejects(
            Deviation::StaleHandleAccepted,
            "a second handle must reject the stale version",
            |fixture| check_multiple_handles(fixture, Tick(1), Tick(2)),
        );
    }
}

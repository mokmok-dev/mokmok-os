//! Shared contract checks every [`EventStore`] adapter must pass.
//!
//! An adapter keeps its own tests for a file, a bucket, or a cache. This module is
//! the overlap: append, load, conflict, listing, and the empty-append rule. A test
//! opens a fresh store and calls [`check`].

#![allow(clippy::expect_used, clippy::panic)]

use std::fmt::Debug;

use crate::StreamKey;
use crate::cloud_event::{CloudEvent, EventType};
use crate::port::{EnumerableStore, EventStore, StoreError};
use crate::stream::Version;

/// Runs every shared store check against `store`.
///
/// `store` must list no streams. `alpha` and `beta` must be distinct unused keys.
/// `first` and `second` are two different events of the adapter's domain.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check<K, E, S>(
    store: &S,
    alpha: &K,
    beta: &K,
    first: E,
    second: E,
) where
    K: StreamKey + Debug,
    E: Clone + Debug + EventType + PartialEq + Send + Sync,
    S: EventStore<K, E> + EnumerableStore<K, E>,
{
    assert_ne!(
        alpha, beta,
        "the two probe keys must name different streams"
    );
    assert_eq!(
        store.keys().expect("a fresh store lists"),
        Vec::<K>::new(),
        "a fresh store lists no streams"
    );

    a_missing_stream_loads_empty(store, alpha);
    an_empty_append_does_not_create_a_stream(store, alpha);
    a_contiguous_append_round_trips(store, alpha, first.clone(), second.clone());
    a_stale_append_is_a_conflict(store, alpha, first.clone());
    a_record_for_another_stream_is_refused(store, beta, alpha, first.clone());
    a_skipped_version_is_refused(store, beta, second);
    two_streams_are_independent(store, alpha, beta, first);
    keys_lists_every_stream_in_key_order(store, alpha, beta);
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
    E: Debug,
    S: EnumerableStore<K, E>,
{
    let mut expected = vec![alpha.clone(), beta.clone()];
    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(store.keys().expect("the store lists its streams"), expected);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::port::check_append;

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct Probe(String);

    impl StreamKey for Probe {
        fn as_str(&self) -> &str {
            self.0.as_str()
        }

        fn parse_key(raw: &str) -> Result<Self, StoreError> {
            Ok(Self(raw.to_owned()))
        }
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct Tick(u64);

    impl EventType for Tick {
        fn event_type(&self) -> &'static str {
            "dev.mokmok.tick"
        }
    }

    struct Memory {
        streams: Mutex<HashMap<Probe, Vec<CloudEvent<Tick>>>>,
    }

    impl Memory {
        fn new() -> Self {
            Self {
                streams: Mutex::new(HashMap::new()),
            }
        }
    }

    impl EventStore<Probe, Tick> for Memory {
        fn load(
            &self,
            key: &Probe,
        ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
            Ok(self
                .streams
                .lock()
                .expect("the probe store is not poisoned")
                .get(key)
                .cloned()
                .unwrap_or_default())
        }

        fn append(
            &self,
            key: &Probe,
            expected: Version,
            events: &[CloudEvent<Tick>],
        ) -> Result<Version, StoreError> {
            let mut streams = self
                .streams
                .lock()
                .expect("the probe store is not poisoned");
            let actual = streams.get(key).map_or(Version::ZERO, |log| {
                Version::new(u64::try_from(log.len()).expect("log fits"))
            });
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

    impl EnumerableStore<Probe, Tick> for Memory {
        fn keys(&self) -> Result<Vec<Probe>, StoreError> {
            let mut keys: Vec<Probe> = self
                .streams
                .lock()
                .expect("the probe store is not poisoned")
                .keys()
                .cloned()
                .collect();
            keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            Ok(keys)
        }
    }

    #[test]
    fn a_well_behaved_store_passes() {
        check(
            &Memory::new(),
            &Probe("alpha".to_owned()),
            &Probe("beta".to_owned()),
            Tick(1),
            Tick(2),
        );
    }

    #[test]
    fn an_empty_append_that_inserts_a_stream_fails_the_contract() {
        struct Ghost(Memory);

        impl EventStore<Probe, Tick> for Ghost {
            fn load(
                &self,
                key: &Probe,
            ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
                self.0.load(key)
            }

            fn append(
                &self,
                key: &Probe,
                expected: Version,
                events: &[CloudEvent<Tick>],
            ) -> Result<Version, StoreError> {
                let result = self.0.append(key, expected, events)?;
                if events.is_empty() {
                    self.0
                        .streams
                        .lock()
                        .expect("the probe store is not poisoned")
                        .entry(key.clone())
                        .or_default();
                }
                Ok(result)
            }
        }

        impl EnumerableStore<Probe, Tick> for Ghost {
            fn keys(&self) -> Result<Vec<Probe>, StoreError> {
                self.0.keys()
            }
        }

        let result = std::panic::catch_unwind(|| {
            check(
                &Ghost(Memory::new()),
                &Probe("alpha".to_owned()),
                &Probe("beta".to_owned()),
                Tick(1),
                Tick(2),
            );
        });
        assert!(
            result.is_err(),
            "a ghost stream from an empty append must fail the contract"
        );
    }
}

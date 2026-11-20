//! The checks every [`EventStore`] adapter must pass.
//!
//! An adapter keeps its own tests for a file, a bucket, or a cache. This crate is the
//! overlap: append, load, conflict, listing, the empty-append rule, the torn tail, stray
//! entries, and two handles over one root. A test opens a fresh store and calls [`check`],
//! or implements [`Backend`] and also runs the checks that need a store root.
//!
//! Nothing in the daemon depends on this crate. Only adapter tests do.

#![allow(clippy::expect_used, clippy::panic)]

use std::fmt::Debug;

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, StoreError, StreamKey, Version,
    encode_records, object_name,
};
use serde::Serialize;

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

/// An adapter that can name a fresh place to keep streams and write bytes a store did not
/// produce.
///
/// [`check`] only needs an [`EventStore`]. The rules about a torn tail, a stray entry, and
/// a second handle over one root need to know where the streams live and to write there
/// without going through [`EventStore::append`], which the store interface cannot say.
pub trait Backend {
    type Key: StreamKey + Debug;
    type Event: EventType + Clone + Debug + PartialEq + Send + Sync + Serialize;
    type Store: EventStore<Self::Key, Self::Event> + EnumerableStore<Self::Key, Self::Event>;

    /// A store over a fresh, empty stream root.
    ///
    /// Every call must name a root no earlier call named, so one check never sees the
    /// records another committed.
    fn store(&self) -> Self::Store;

    /// A second store over the same root as an earlier [`Backend::store`], or `None` when
    /// the adapter keeps its state in memory and has no root to share.
    ///
    /// The root is the one the most recent [`Backend::store`] named. The two stores keep
    /// separate handles on the same records, as two processes over one root would.
    fn reopen(&self) -> Option<Self::Store>;

    /// Writes `bytes` to the place a stream named `name` occupies, bypassing `append`.
    ///
    /// `name` is what [`object_name`] returns for a key, or a name no stream uses. The
    /// write lands in the root the most recent [`Backend::store`] or [`Backend::reopen`]
    /// named.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the bytes cannot be written.
    fn write_raw(
        &self,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError>;
}

/// Pins that an uncommitted tail is neither read nor kept.
///
/// A writer that dies mid-record leaves bytes past the last newline. A reader must return
/// only the committed records, and the next append must drop the partial record rather
/// than write after it. The read goes through a handle opened after the tail appeared,
/// which is the handle a restarted process holds.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check_torn_tail<B: Backend>(
    backend: &B,
    key: &B::Key,
    first: B::Event,
    second: B::Event,
) {
    let opened = backend.store();
    let one = record(key, 1, first);
    let two = record(key, 2, second);
    opened
        .append(key, Version::ZERO, std::slice::from_ref(&one))
        .expect("the first append commits");

    let mut torn = committed_bytes(std::slice::from_ref(&one));
    torn.extend_from_slice(b"{\"specversion\":\"1.0\"");
    backend
        .write_raw(&object_name(key.as_str()), &torn)
        .expect("the torn write lands where the stream lives");

    let reopened = backend.reopen();
    let store = reopened.as_ref().unwrap_or(&opened);
    assert_eq!(
        store.load(key).expect("the torn stream reads"),
        vec![one.clone()],
        "an uncommitted tail is not a record"
    );
    assert_eq!(
        store
            .append(key, Version::new(1), std::slice::from_ref(&two))
            .expect("an append after a torn tail commits"),
        Version::new(2),
        "a torn tail must not advance the version"
    );
    assert_eq!(
        store.load(key).expect("the repaired stream reads"),
        vec![one, two],
        "the next append must drop the tail and write one record"
    );
}

/// Pins that listing names only the streams the adapter holds.
///
/// A name that is not a canonical stream object, such as `notes.txt` or a `.jsonl` object
/// whose stem is not a key, must not appear in [`EnumerableStore::keys`].
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check_stray_entries<B: Backend>(
    backend: &B,
    key: &B::Key,
    first: B::Event,
) {
    let store = backend.store();
    store
        .append(key, Version::ZERO, &[record(key, 1, first)])
        .expect("the real stream commits");
    backend
        .write_raw("notes.txt", b"stray")
        .expect("a stray name writes");
    backend
        .write_raw("not-a-key.jsonl", b"stray")
        .expect("a non-canonical name writes");
    assert_eq!(
        store.keys().expect("the store lists"),
        vec![key.clone()],
        "listing must name only the streams the store holds"
    );
}

/// Pins that two handles over one root share one version sequence.
///
/// Skipped when [`Backend::reopen`] returns `None`, because an adapter with no root cannot
/// be opened twice. A handle that trusts the version it read earlier can append after the
/// other handle moved on, which puts two records at one version and leaves the log
/// unreadable.
///
/// # Panics
///
/// Panics when a check fails, so the cargo test names the contract that broke.
pub fn check_shared_root<B: Backend>(
    backend: &B,
    key: &B::Key,
    first: B::Event,
    second: B::Event,
    third: B::Event,
) {
    let older = backend.store();
    let Some(newer) = backend.reopen() else {
        return;
    };
    older
        .append(key, Version::ZERO, &[record(key, 1, first)])
        .expect("the first handle commits version 1");
    newer
        .append(key, Version::new(1), &[record(key, 2, second)])
        .expect("the second handle commits version 2");
    let error = older
        .append(key, Version::new(1), &[record(key, 2, third)])
        .expect_err("the first handle must see the version the second committed");
    assert!(
        matches!(
            error,
            StoreError::Conflict {
                expected,
                actual,
            } if expected == Version::new(1) && actual == Version::new(2)
        ),
        "a second handle must observe every committed version: {error:?}"
    );
    assert_eq!(
        older.load(key).expect("the shared log reads").len(),
        2,
        "the refused append must not add a record"
    );
}

fn record<K: StreamKey, E: EventType>(
    key: &K,
    version: u64,
    data: E,
) -> CloudEvent<E> {
    CloudEvent::new(key.as_str(), Version::new(version), data)
}

fn committed_bytes<E: Serialize>(events: &[CloudEvent<E>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_records(events, &mut bytes).expect("a probe record encodes");
    bytes
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
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex, MutexGuard};

    use mokmok_events::{
        OBJECT_SUFFIX, canonical_keys, check_append, complete_prefix, parse_records,
    };
    use serde::{Deserialize, Serialize};

    use super::*;

    /// The bytes one root holds, named as [`object_name`] names them.
    type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct Probe(String);

    impl Probe {
        fn new(name: &str) -> Self {
            Self(name.to_owned())
        }
    }

    impl StreamKey for Probe {
        fn as_str(&self) -> &str {
            self.0.as_str()
        }

        fn parse_key(raw: &str) -> Result<Self, StoreError> {
            if !raw.is_empty() && raw.chars().all(|raw| raw.is_ascii_lowercase()) {
                Ok(Self(raw.to_owned()))
            } else {
                Err(StoreError::Invalid(raw.to_owned()))
            }
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
        objects: Objects,
    }

    impl Memory {
        fn objects(&self) -> MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
            self.objects
                .lock()
                .expect("the probe store is not poisoned")
        }

        fn stored_version(
            &self,
            key: &Probe,
        ) -> Result<Version, StoreError> {
            let bytes = self
                .objects()
                .get(&object_name(key.as_str()))
                .map(|bytes| complete_prefix(bytes).to_vec());
            let Some(bytes) = bytes else {
                return Ok(Version::ZERO);
            };
            parse_records::<Tick>("probe", key.as_str(), &bytes).map(|(version, _)| version)
        }

        /// Commits `events` after the committed prefix, dropping any tail.
        fn push(
            &self,
            key: &Probe,
            events: &[CloudEvent<Tick>],
        ) -> Result<(), StoreError> {
            let name = object_name(key.as_str());
            let mut committed = self
                .objects()
                .get(&name)
                .map(|bytes| complete_prefix(bytes).to_vec())
                .unwrap_or_default();
            encode_records(events, &mut committed)?;
            self.objects().insert(name, committed);
            Ok(())
        }
    }

    impl EventStore<Probe, Tick> for Memory {
        fn load(
            &self,
            key: &Probe,
        ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
            let bytes = self
                .objects()
                .get(&object_name(key.as_str()))
                .map(|bytes| complete_prefix(bytes).to_vec());
            let Some(bytes) = bytes else {
                return Ok(Vec::new());
            };
            parse_records("probe", key.as_str(), &bytes).map(|(_, events)| events)
        }

        fn append(
            &self,
            key: &Probe,
            expected: Version,
            events: &[CloudEvent<Tick>],
        ) -> Result<Version, StoreError> {
            let current = self.stored_version(key)?;
            if current != expected {
                return Err(StoreError::Conflict {
                    expected,
                    actual: current,
                });
            }
            if events.is_empty() {
                return Ok(current);
            }
            let last = check_append(key.as_str(), expected, events)?;
            self.push(key, events)?;
            Ok(last)
        }
    }

    impl EnumerableStore<Probe, Tick> for Memory {
        fn keys(&self) -> Result<Vec<Probe>, StoreError> {
            Ok(canonical_keys::<Probe>(
                self.objects().keys().map(String::as_str),
            ))
        }
    }

    /// A [`Backend`] that keeps every root it opened, so `reopen` and `write_raw` reach
    /// the same bytes as the last `store`.
    struct Fixture<S> {
        roots: Mutex<Vec<Objects>>,
        build: fn(Objects) -> S,
    }

    impl<S> Fixture<S> {
        fn new(build: fn(Objects) -> S) -> Self {
            Self {
                roots: Mutex::new(Vec::new()),
                build,
            }
        }

        fn current(&self) -> Option<Objects> {
            self.roots
                .lock()
                .expect("the fixture is not poisoned")
                .last()
                .cloned()
        }
    }

    impl<S> Backend for Fixture<S>
    where
        S: EventStore<Probe, Tick> + EnumerableStore<Probe, Tick>,
    {
        type Key = Probe;
        type Event = Tick;
        type Store = S;

        fn store(&self) -> S {
            let objects = Objects::default();
            self.roots
                .lock()
                .expect("the fixture is not poisoned")
                .push(Arc::clone(&objects));
            (self.build)(objects)
        }

        fn reopen(&self) -> Option<S> {
            let objects = self.current()?;
            Some((self.build)(objects))
        }

        fn write_raw(
            &self,
            name: &str,
            bytes: &[u8],
        ) -> Result<(), StoreError> {
            let objects = self
                .current()
                .ok_or_else(|| StoreError::Io("no root is open".to_owned()))?;
            objects
                .lock()
                .expect("the probe store is not poisoned")
                .insert(name.to_owned(), bytes.to_vec());
            Ok(())
        }
    }

    fn fixture() -> Fixture<Memory> {
        Fixture::new(|objects| Memory { objects })
    }

    fn alpha() -> Probe {
        Probe::new("alpha")
    }

    fn beta() -> Probe {
        Probe::new("beta")
    }

    #[test]
    fn a_well_behaved_store_passes_every_check() {
        let fixture = fixture();
        check(&fixture.store(), &alpha(), &beta(), Tick(1), Tick(2));
        check_torn_tail(&fixture, &alpha(), Tick(1), Tick(2));
        check_stray_entries(&fixture, &alpha(), Tick(1));
        check_shared_root(&fixture, &alpha(), Tick(1), Tick(2), Tick(3));
    }

    #[test]
    fn a_backend_with_no_root_passes_every_check() {
        struct Volatile(Fixture<Memory>);

        impl Backend for Volatile {
            type Key = Probe;
            type Event = Tick;
            type Store = Memory;

            fn store(&self) -> Memory {
                self.0.store()
            }

            fn reopen(&self) -> Option<Memory> {
                None
            }

            fn write_raw(
                &self,
                name: &str,
                bytes: &[u8],
            ) -> Result<(), StoreError> {
                self.0.write_raw(name, bytes)
            }
        }

        let backend = Volatile(fixture());
        check(&backend.store(), &alpha(), &beta(), Tick(1), Tick(2));
        check_torn_tail(&backend, &alpha(), Tick(1), Tick(2));
        check_stray_entries(&backend, &alpha(), Tick(1));
        check_shared_root(&backend, &alpha(), Tick(1), Tick(2), Tick(3));
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
                        .objects()
                        .entry(object_name(key.as_str()))
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
            let fixture = Fixture::new(|objects| Ghost(Memory { objects }));
            check(&fixture.store(), &alpha(), &beta(), Tick(1), Tick(2));
        });
        assert!(
            result.is_err(),
            "a ghost stream from an empty append must fail the contract"
        );
    }

    #[test]
    fn a_torn_tail_that_is_parsed_fails_the_contract() {
        /// Reads and writes the whole object, so the tail counts as a record and the next
        /// append lands after it.
        struct Ghost(Memory);

        impl EventStore<Probe, Tick> for Ghost {
            fn load(
                &self,
                key: &Probe,
            ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
                let bytes = self.0.objects().get(&object_name(key.as_str())).cloned();
                let Some(bytes) = bytes else {
                    return Ok(Vec::new());
                };
                parse_records("probe", key.as_str(), &bytes).map(|(_, events)| events)
            }

            fn append(
                &self,
                key: &Probe,
                expected: Version,
                events: &[CloudEvent<Tick>],
            ) -> Result<Version, StoreError> {
                let current = self.0.stored_version(key)?;
                if current != expected {
                    return Err(StoreError::Conflict {
                        expected,
                        actual: current,
                    });
                }
                if events.is_empty() {
                    return Ok(current);
                }
                let last = check_append(key.as_str(), expected, events)?;
                let mut objects = self.0.objects();
                let name = object_name(key.as_str());
                let mut bytes = objects.remove(&name).unwrap_or_default();
                encode_records(events, &mut bytes)?;
                objects.insert(name, bytes);
                drop(objects);
                Ok(last)
            }
        }

        impl EnumerableStore<Probe, Tick> for Ghost {
            fn keys(&self) -> Result<Vec<Probe>, StoreError> {
                self.0.keys()
            }
        }

        let result = std::panic::catch_unwind(|| {
            let fixture = Fixture::new(|objects| Ghost(Memory { objects }));
            check(&fixture.store(), &alpha(), &beta(), Tick(1), Tick(2));
            check_torn_tail(&fixture, &alpha(), Tick(1), Tick(2));
        });
        assert!(
            result.is_err(),
            "a store that parses a torn tail must fail the contract"
        );
    }

    #[test]
    fn a_listing_that_names_a_stray_entry_fails_the_contract() {
        /// Parses every object name as a key, without the stream suffix or the key form.
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
                self.0.append(key, expected, events)
            }
        }

        impl EnumerableStore<Probe, Tick> for Ghost {
            fn keys(&self) -> Result<Vec<Probe>, StoreError> {
                let names: Vec<String> = self.0.objects().keys().cloned().collect();
                let mut keys: Vec<Probe> = names
                    .into_iter()
                    .map(|name| {
                        let stem = name
                            .strip_suffix(OBJECT_SUFFIX)
                            .map_or_else(|| name.clone(), str::to_owned);
                        Probe(stem)
                    })
                    .collect();
                keys.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                Ok(keys)
            }
        }

        let result = std::panic::catch_unwind(|| {
            let fixture = Fixture::new(|objects| Ghost(Memory { objects }));
            check(&fixture.store(), &alpha(), &beta(), Tick(1), Tick(2));
            check_stray_entries(&fixture, &alpha(), Tick(1));
        });
        assert!(
            result.is_err(),
            "a listing that names a stray entry must fail the contract"
        );
    }

    #[test]
    fn a_second_handle_that_trusts_a_stale_cache_fails_the_contract() {
        /// Checks an append against the version this handle last saw, not the one the
        /// object now holds.
        struct Ghost {
            inner: Memory,
            versions: Mutex<HashMap<Probe, Version>>,
        }

        impl EventStore<Probe, Tick> for Ghost {
            fn load(
                &self,
                key: &Probe,
            ) -> Result<Vec<CloudEvent<Tick>>, StoreError> {
                self.inner.load(key)
            }

            fn append(
                &self,
                key: &Probe,
                expected: Version,
                events: &[CloudEvent<Tick>],
            ) -> Result<Version, StoreError> {
                let mut versions = self.versions.lock().expect("the ghost is not poisoned");
                let current = match versions.get(key) {
                    Some(version) => *version,
                    None => self.inner.stored_version(key)?,
                };
                if current != expected {
                    return Err(StoreError::Conflict {
                        expected,
                        actual: current,
                    });
                }
                if events.is_empty() {
                    return Ok(current);
                }
                let last = check_append(key.as_str(), expected, events)?;
                self.inner.push(key, events)?;
                versions.insert(key.clone(), last);
                drop(versions);
                Ok(last)
            }
        }

        impl EnumerableStore<Probe, Tick> for Ghost {
            fn keys(&self) -> Result<Vec<Probe>, StoreError> {
                self.inner.keys()
            }
        }

        let result = std::panic::catch_unwind(|| {
            let fixture = Fixture::new(|objects| Ghost {
                inner: Memory { objects },
                versions: Mutex::new(HashMap::new()),
            });
            check(&fixture.store(), &alpha(), &beta(), Tick(1), Tick(2));
            check_shared_root(&fixture, &alpha(), Tick(1), Tick(2), Tick(3));
        });
        assert!(
            result.is_err(),
            "a second handle with a stale cache must fail the contract"
        );
    }
}

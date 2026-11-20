//! JSONL platform adapter for `mokmok-events`: one append-only file per stream.
//!
//! The file stem is [`StreamKey::as_str`]. Any key that writes a single safe path
//! component can use this adapter, and `UUID` keys (such as the jobs crate's
//! `JobId`) are the common case.
//!
//! Each line is one `CloudEvents` 1.0 JSON record from `mokmok-events`. The file
//! holds no header and nothing else, so the same record shape is what the in-memory
//! adapter keeps.
//!
//! One store instance owns a root. The map of streams is an `RwLock` held only
//! while looking up or inserting a stream. Each stream then has its own mutex
//! and version cache, so an append to one file does not hold up another. The
//! cache also remembers the file's length, so a second handle over the same
//! root sees the bytes the first one appended and re-reads the version instead
//! of committing a duplicate. Two appends that pass that check at the same
//! moment can still both write, so share one `Arc<JsonlEventStore>` across every
//! actor system over that root.
//!
//! Correctness across processes is not claimed; the logical fence is the
//! store's version check.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, StoreError, StreamKey, Version,
    canonical_keys, check_append, complete_prefix, encode_records, object_name, parse_records,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct JsonlEventStore<K, E> {
    root: PathBuf,
    streams: RwLock<HashMap<K, Arc<StreamState>>>,
    event: PhantomData<fn() -> E>,
}

#[derive(Debug)]
struct StreamState {
    cache: Mutex<Option<Cache>>,
}

/// The version this handle last read or wrote, and the bytes the file held then.
///
/// The length is what makes the cache safe with more than one handle over a root: an
/// append that finds the file longer than this no longer knows its version.
#[derive(Clone, Copy, Debug)]
struct Cache {
    version: Version,
    length: u64,
}

impl<K, E> JsonlEventStore<K, E> {
    /// Opens or creates the root directory. No stream is read until it is
    /// touched. A stream's file is named `<key>.jsonl`, where `<key>` is
    /// [`StreamKey::as_str`] (a canonical lowercase UUID for `JobId`).
    ///
    /// One instance owns a root. The stream map is an `RwLock` held only to look
    /// up or insert an entry. Each stream's version cache lives behind that
    /// stream's mutex. A second handle over the same root sees the bytes the
    /// first one appended and re-reads the version rather than committing a
    /// duplicate, but two appends that pass that check together can still both
    /// write, so share one `Arc<JsonlEventStore>` across every actor system.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the root cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| error_at(&root, &error))?;
        Ok(Self {
            root,
            streams: RwLock::new(HashMap::new()),
            event: PhantomData,
        })
    }

    fn stream_state(
        &self,
        key: &K,
    ) -> Result<Arc<StreamState>, StoreError>
    where
        K: StreamKey,
    {
        {
            let streams = self.streams.read().map_err(|_| lock_poisoned())?;
            if let Some(state) = streams.get(key) {
                return Ok(Arc::clone(state));
            }
        }
        let mut streams = self.streams.write().map_err(|_| lock_poisoned())?;
        Ok(Arc::clone(streams.entry(key.clone()).or_insert_with(
            || {
                Arc::new(StreamState {
                    cache: Mutex::new(None),
                })
            },
        )))
    }

    fn with_stream<R>(
        &self,
        key: &K,
        body: impl FnOnce(&mut Option<Cache>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError>
    where
        K: StreamKey,
    {
        let state = self.stream_state(key)?;
        let mut cache = state.cache.lock().map_err(|_| lock_poisoned())?;
        body(&mut cache)
    }

    fn path(
        &self,
        key: &K,
    ) -> PathBuf
    where
        K: StreamKey,
    {
        self.root.join(object_name(key.as_str()))
    }
}

fn ensure<E>(
    path: &Path,
    subject: &str,
) -> Result<(Cache, File), StoreError>
where
    E: EventType + DeserializeOwned,
{
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(|error| error_at(path, &error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error_at(path, &error))?;
    let committed = complete_prefix(&bytes).len();
    if committed != bytes.len() {
        let length = u64::try_from(committed).map_err(|error| StoreError::Io(error.to_string()))?;
        file.set_len(length)
            .map_err(|error| error_at(path, &error))?;
        // The truncation must survive a crash before the next append, or the torn tail returns.
        file.sync_data().map_err(|error| error_at(path, &error))?;
    }
    let (version, _) =
        parse_records::<E>(&path.display().to_string(), subject, &bytes[..committed])?;
    let length = u64::try_from(committed).map_err(|error| StoreError::Io(error.to_string()))?;
    Ok((Cache { version, length }, file))
}

impl<K, E> EventStore<K, E> for JsonlEventStore<K, E>
where
    K: StreamKey,
    E: EventType + Serialize + DeserializeOwned,
{
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<CloudEvent<E>>, StoreError> {
        let path = self.path(key);
        self.with_stream(key, |_| {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                },
                Err(error) => return Err(error_at(&path, &error)),
            };
            let (_, events) = parse_records(
                &path.display().to_string(),
                key.as_str(),
                complete_prefix(&bytes),
            )?;
            Ok(events)
        })
    }

    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[CloudEvent<E>],
    ) -> Result<Version, StoreError> {
        let path = self.path(key);
        self.with_stream(key, |cache| {
            // A cached version describes the file only while the file still ends where
            // this handle left it. Another handle over the same root moves that end.
            let mut at_tip = None;
            if let Some(seen) = *cache
                && let Ok(file) = OpenOptions::new().append(true).open(&path)
                && file.metadata().is_ok_and(|meta| meta.len() == seen.length)
            {
                at_tip = Some((seen, file));
            }
            let current = if let Some((seen, _)) = at_tip {
                seen.version
            } else {
                // Whatever this handle last saw, the file is the authority. Read it now,
                // so a stale append becomes the conflict it already is.
                let stored = stored_stream::<E>(&path, key.as_str())?;
                *cache = Some(stored);
                stored.version
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
            // A handle kept in `at_tip` already sits at the file's end. A handle that had
            // to re-read opens through `ensure`, which drops a torn tail and returns the
            // version it finds; that version decides, because the file could have moved
            // between the read above and this open.
            let (before, mut file) = if let Some((seen, file)) = at_tip {
                (seen, file)
            } else {
                let (stored, file) = ensure::<E>(&path, key.as_str())?;
                *cache = Some(stored);
                if stored.version != expected {
                    return Err(StoreError::Conflict {
                        expected,
                        actual: stored.version,
                    });
                }
                (stored, file)
            };
            let mut buffer = Vec::new();
            encode_records(events, &mut buffer)?;
            // A failed write needs no invalidation: if a byte landed, the file is longer
            // than `before` and the next append re-reads it. If none did, this cache still
            // describes the file.
            file.write_all(&buffer)
                .and_then(|()| file.sync_data())
                .map_err(|error| error_at(&path, &error))?;
            let written =
                u64::try_from(buffer.len()).map_err(|error| StoreError::Io(error.to_string()))?;
            *cache = Some(Cache {
                version: last,
                length: before.length + written,
            });
            Ok(last)
        })
    }
}

impl<K, E> EnumerableStore<K, E> for JsonlEventStore<K, E>
where
    K: StreamKey,
    E: EventType + Serialize + DeserializeOwned,
{
    /// Lists the names that round-trip through [`StreamKey::parse_key`], skipping any
    /// other entry in the directory.
    fn keys(&self) -> Result<Vec<K>, StoreError> {
        let mut names = Vec::new();
        let entries = fs::read_dir(&self.root).map_err(|error| error_at(&self.root, &error))?;
        for entry in entries {
            let entry = entry.map_err(|error| error_at(&self.root, &error))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|error| error_at(&path, &error))?
                .is_file()
            {
                continue;
            }
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(canonical_keys(names))
    }
}

fn stored_stream<E>(
    path: &Path,
    subject: &str,
) -> Result<Cache, StoreError>
where
    E: EventType + DeserializeOwned,
{
    match fs::read(path) {
        Ok(bytes) => {
            let committed = complete_prefix(&bytes);
            let (version, _) = parse_records::<E>(&path.display().to_string(), subject, committed)?;
            let length = u64::try_from(committed.len())
                .map_err(|error| StoreError::Io(error.to_string()))?;
            Ok(Cache { version, length })
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cache {
            version: Version::ZERO,
            length: 0,
        }),
        Err(error) => Err(error_at(path, &error)),
    }
}

fn error_at(
    path: &Path,
    error: &std::io::Error,
) -> StoreError {
    StoreError::Io(format!("{}: {error}", path.display()))
}

fn lock_poisoned() -> StoreError {
    StoreError::Io("event store lock poisoned".to_owned())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use mokmok_events::{CloudEvent, EventType};
    use mokmok_jobs_core::{JobEvent, JobId};
    use serde::{Deserialize, Serialize};

    use super::*;

    fn id(n: u128) -> JobId {
        JobId::from_u128(n)
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

    fn first(id: &JobId) -> CloudEvent<JobEvent> {
        at(id, 1, submitted())
    }

    fn line(record: &CloudEvent<JobEvent>) -> String {
        serde_json::to_string(record).expect("record encodes")
    }

    fn store(directory: &tempfile::TempDir) -> JsonlEventStore<JobId, JobEvent> {
        JsonlEventStore::open(directory.path().join("jobs")).expect("store opens")
    }

    #[test]
    fn the_store_meets_the_event_store_contract() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        mokmok_events::conformance::check(
            &store(&directory),
            &id(1),
            &id(2),
            submitted(),
            JobEvent::Started { attempt: 1 },
        );
        Ok(())
    }

    #[test]
    fn open_creates_missing_parent_directories() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("nested").join("deeper");
        let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        let id = id(1);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        assert_eq!(store.load(&id)?.len(), 1);
        assert!(root.is_dir());
        Ok(())
    }

    #[test]
    fn independent_handles_commit_to_distinct_streams_together()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let results = std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for n in 40..56 {
                let root = root.clone();
                joins.push(scope.spawn(move || {
                    let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
                    let id = id(n);
                    store.append(&id, Version::ZERO, &[first(&id)])?;
                    Ok::<_, StoreError>(())
                }));
            }
            joins
                .into_iter()
                .map(|join| join.join().expect("append thread"))
                .collect::<Vec<_>>()
        });
        for result in results {
            result.expect("each handle commits its own stream");
        }

        let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        assert_eq!(store.keys()?.len(), 16);
        Ok(())
    }

    #[test]
    fn concurrent_appends_to_distinct_streams_all_commit() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(store(&directory));
        std::thread::scope(|scope| {
            for n in 40..56 {
                let store = Arc::clone(&store);
                scope.spawn(move || {
                    let id = id(n);
                    store
                        .append(&id, Version::ZERO, &[first(&id)])
                        .expect("each stream commits");
                });
            }
        });
        assert_eq!(store.keys()?.len(), 16);
        Ok(())
    }

    #[test]
    fn concurrent_appends_to_the_same_stream_conflict() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(store(&directory));
        let id = id(41);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for _ in 0..8 {
                let store = Arc::clone(&store);
                let barrier = &barrier;
                let id = id.clone();
                joins.push(scope.spawn(move || {
                    barrier.wait();
                    store.append(
                        &id,
                        Version::new(1),
                        &[at(&id, 2, JobEvent::Started { attempt: 1 })],
                    )
                }));
            }
            joins
                .into_iter()
                .map(|join| join.join().expect("append thread"))
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        for result in &results {
            if result.is_err() {
                assert!(matches!(
                    result,
                    Err(StoreError::Conflict {
                        expected,
                        actual,
                    }) if *expected == Version::new(1) && *actual == Version::new(2)
                ));
            }
        }
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn an_append_blocked_on_one_stream_does_not_hold_up_another()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir()?;
        let store = Arc::new(store(&directory));
        let blocked_id = id(42);
        let free_id = id(43);
        store.append(&blocked_id, Version::ZERO, &[first(&blocked_id)])?;

        // Holding one stream's fence is what a slow append does. A sibling that commits
        // while it is held proves the two streams do not share one.
        let fence = store.stream_state(&blocked_id)?;
        let held = fence
            .cache
            .lock()
            .map_err(|_| "the fence is not poisoned")?;
        let stalled = {
            let store = Arc::clone(&store);
            let blocked_id = blocked_id.clone();
            std::thread::spawn(move || {
                store.append(
                    &blocked_id,
                    Version::new(1),
                    &[at(&blocked_id, 2, JobEvent::Started { attempt: 1 })],
                )
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !stalled.is_finished(),
            "the stalled append returned before the sibling ran"
        );

        let started = Instant::now();
        store.append(&free_id, Version::ZERO, &[first(&free_id)])?;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a sibling stream must commit while the other append is still blocked"
        );
        assert_eq!(store.load(&free_id)?.len(), 1);

        drop(held);
        let version = stalled.join().expect("the stalled append thread")?;
        assert_eq!(version, Version::new(2));
        assert_eq!(store.load(&blocked_id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn streams_live_in_separate_files() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let first_id = id(1);
        let second_id = id(2);
        store.append(&first_id, Version::ZERO, &[first(&first_id)])?;
        store.append(
            &second_id,
            Version::ZERO,
            &[at(&second_id, 1, JobEvent::Started { attempt: 1 })],
        )?;
        assert_ne!(store.path(&first_id), store.path(&second_id));
        assert_eq!(store.load(&first_id)?.len(), 1);
        assert_eq!(store.load(&second_id)?.len(), 1);
        assert_ne!(
            store.load(&first_id)?[0].data,
            store.load(&second_id)?[0].data
        );
        Ok(())
    }

    #[test]
    fn reopening_twice_keeps_every_committed_record() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let id = id(3);
        {
            let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
            store.append(&id, Version::ZERO, &[first(&id)])?;
            store.append(
                &id,
                Version::new(1),
                &[at(&id, 2, JobEvent::Started { attempt: 1 })],
            )?;
        }
        {
            drop(JsonlEventStore::<JobId, JobEvent>::open(&root)?);
        }
        let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn independent_handles_reject_stale_appends() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let older = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        let newer = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        let id = id(60);

        older.append(&id, Version::ZERO, &[first(&id)])?;
        newer.append(
            &id,
            Version::new(1),
            &[at(&id, 2, JobEvent::Started { attempt: 1 })],
        )?;

        let stale = older.append(
            &id,
            Version::new(1),
            &[at(&id, 2, JobEvent::Started { attempt: 1 })],
        );
        assert!(
            matches!(
                stale,
                Err(StoreError::Conflict {
                    expected,
                    actual,
                }) if expected == Version::new(1) && actual == Version::new(2)
            ),
            "the older handle must refuse the version the newer one took: {stale:?}"
        );
        assert_eq!(
            older.load(&id)?.len(),
            2,
            "the refused append must leave the history alone"
        );
        Ok(())
    }

    #[test]
    fn opening_an_existing_stream_keeps_its_version() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let id = id(4);
        {
            let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
            store.append(&id, Version::ZERO, &[first(&id)])?;
        }
        let store = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        store.append(
            &id,
            Version::new(1),
            &[at(&id, 2, JobEvent::Started { attempt: 1 })],
        )?;
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn torn_tail_is_discarded_before_the_next_append() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let writer = store(&directory);
        let id = id(5);
        writer.append(&id, Version::ZERO, &[first(&id)])?;
        let mut file = OpenOptions::new().append(true).open(writer.path(&id))?;
        file.write_all(format!("{{\"specversion\":\"1.0\",\"id\":\"{}", id.as_str()).as_bytes())?;
        drop(file);
        let store = store(&directory);
        assert_eq!(store.load(&id)?.len(), 1);
        let version = store.append(
            &id,
            Version::new(1),
            &[at(&id, 2, JobEvent::Started { attempt: 1 })],
        )?;
        assert_eq!(version, Version::new(2));
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn a_handle_that_cached_the_file_still_drops_another_writers_torn_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let id = id(61);
        let reader = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        let writer = JsonlEventStore::<JobId, JobEvent>::open(&root)?;
        writer.append(&id, Version::ZERO, &[first(&id)])?;
        // The reader now caches version 1 at this file's length.
        reader.append(&id, Version::new(1), &[])?;

        let mut file = OpenOptions::new().append(true).open(reader.path(&id))?;
        file.write_all(b"{\"specversion\":\"1.0\"")?;
        drop(file);

        // The file is longer than the reader cached, so the reader re-reads and writes
        // through `ensure`, which drops the tail the other writer left.
        let version = reader.append(
            &id,
            Version::new(1),
            &[at(&id, 2, JobEvent::Started { attempt: 1 })],
        )?;
        assert_eq!(version, Version::new(2));
        assert_eq!(
            fs::read_to_string(reader.path(&id))?.lines().count(),
            2,
            "the torn tail is gone, so the file holds exactly its two records"
        );
        Ok(())
    }

    #[test]
    fn a_torn_first_record_is_replaced_on_append() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(6);
        fs::write(store.path(&id), b"{\"specversion\":\"1.0\"")?;
        assert_eq!(store.load(&id)?.len(), 0);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        assert_eq!(store.load(&id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn an_empty_append_reports_the_current_version_without_creating_a_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(8);
        assert_eq!(store.append(&id, Version::ZERO, &[])?, Version::ZERO);
        assert!(!store.path(&id).exists());
        store.append(&id, Version::ZERO, &[first(&id)])?;
        assert_eq!(store.append(&id, Version::new(1), &[])?, Version::new(1));
        Ok(())
    }

    #[test]
    fn a_record_for_another_stream_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let other = id(11);
        let id = id(10);
        fs::write(store.path(&id), format!("{}\n", line(&first(&other))))?;
        let error = store
            .load(&id)
            .expect_err("a record from another stream is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn an_unsupported_specversion_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(12);
        let mut value = serde_json::to_value(first(&id))?;
        value["specversion"] = serde_json::Value::String("0.3".to_owned());
        fs::write(store.path(&id), format!("{value}\n"))?;
        let error = store
            .load(&id)
            .expect_err("a foreign specversion is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn a_gap_in_the_sequence_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(13);
        fs::write(
            store.path(&id),
            format!("{}\n", line(&at(&id, 2, submitted()))),
        )?;
        let error = store.load(&id).expect_err("a gap is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn a_malformed_complete_line_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(14);
        fs::write(store.path(&id), b"not json\n")?;
        let error = store.load(&id).expect_err("malformed line is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn an_envelope_type_that_disagrees_with_the_data_is_corrupt()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(16);
        let mut value = serde_json::to_value(first(&id))?;
        value["type"] = serde_json::Value::String("dev.mokmok.job.failed".to_owned());
        fs::write(store.path(&id), format!("{value}\n"))?;
        let error = store.load(&id).expect_err("a mismatched type is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn an_id_that_is_not_the_stream_and_version_is_corrupt()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(17);
        let mut value = serde_json::to_value(first(&id))?;
        value["id"] = serde_json::Value::String("another-stream:1".to_owned());
        fs::write(store.path(&id), format!("{value}\n"))?;
        let error = store.load(&id).expect_err("a mismatched id is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn a_stored_line_is_a_cloud_event() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(15);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        let text = fs::read_to_string(store.path(&id))?;
        let record: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("record line"))?;
        assert_eq!(record["specversion"], "1.0");
        assert_eq!(record["id"], format!("{}:1", id.as_str()));
        assert_eq!(record["source"], "mokmok://eventstore");
        assert_eq!(record["type"], "dev.mokmok.job.submitted");
        assert_eq!(record["subject"], id.as_str());
        assert_eq!(record["sequence"], "1");
        assert_eq!(record["sequencetype"], "Integer");
        assert_eq!(record["data"]["kind"], "submitted");
        Ok(())
    }

    #[test]
    fn the_adapter_is_generic_over_key_and_event() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        struct NoteId(String);

        impl StreamKey for NoteId {
            fn as_str(&self) -> &str {
                self.0.as_str()
            }

            fn parse_key(raw: &str) -> Result<Self, StoreError> {
                Ok(Self(raw.to_owned()))
            }
        }

        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        struct Note {
            text: String,
        }

        impl EventType for Note {
            fn event_type(&self) -> &'static str {
                "dev.mokmok.note"
            }
        }

        let directory = tempfile::tempdir()?;
        let store = JsonlEventStore::<NoteId, Note>::open(directory.path().join("notes"))?;
        let id = NoteId("note-1".to_owned());
        let record = CloudEvent::new(
            id.as_str(),
            Version::new(1),
            Note {
                text: "hello".to_owned(),
            },
        );
        store.append(&id, Version::ZERO, &[record])?;
        assert_eq!(store.load(&id)?[0].data.text, "hello");
        Ok(())
    }

    #[test]
    fn a_read_error_that_is_not_missing_is_reported() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(19);
        fs::create_dir(store.path(&id))?;
        let error = store
            .load(&id)
            .expect_err("a directory is not a stream file");
        assert!(matches!(error, StoreError::Io(_)));
        Ok(())
    }

    #[test]
    fn an_empty_append_reports_a_read_error_that_is_not_missing()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(20);
        fs::create_dir(store.path(&id))?;
        let error = store
            .append(&id, Version::ZERO, &[])
            .expect_err("a directory is not a stream file");
        assert!(matches!(error, StoreError::Io(_)));
        Ok(())
    }

    #[test]
    fn deleting_a_cached_stream_forces_a_fresh_version() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(21);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        fs::remove_file(store.path(&id))?;
        let error = store
            .append(
                &id,
                Version::new(1),
                &[at(&id, 2, JobEvent::Started { attempt: 1 })],
            )
            .expect_err("the cached file is gone");
        assert!(matches!(
            error,
            StoreError::Conflict {
                expected,
                actual,
            } if expected == Version::new(1) && actual == Version::ZERO
        ));
        Ok(())
    }

    /// A `write_all` to `/dev/null` succeeds and `sync_data` fails with `EINVAL`, so this
    /// reaches the failure branch without a real disk error.
    #[cfg(unix)]
    fn redirect_for_one_sync(
        store: &JsonlEventStore<JobId, JobEvent>,
        id: &JobId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = store.path(id);
        fs::rename(&path, path.with_extension("held"))?;
        std::os::unix::fs::symlink("/dev/null", &path)?;
        Ok(())
    }

    #[cfg(unix)]
    fn restore_stream(
        store: &JsonlEventStore<JobId, JobEvent>,
        id: &JobId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = store.path(id);
        fs::remove_file(&path)?;
        fs::rename(path.with_extension("held"), &path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_is_reported_and_a_record_it_never_saw_is_not_duplicated()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(30);
        // An empty append caches the empty file, so the redirect's length of zero is what
        // this handle last saw and the append below reaches the write.
        fs::write(store.path(&id), b"")?;
        store.append(&id, Version::ZERO, &[])?;
        redirect_for_one_sync(&store, &id)?;
        let error = store.append(
            &id,
            Version::ZERO,
            &[at(&id, 1, JobEvent::Started { attempt: 1 })],
        );
        restore_stream(&store, &id)?;
        assert!(
            matches!(error, Err(StoreError::Io(_))),
            "the failed sync must surface: {error:?}"
        );

        // A record this handle never saw reaches the file afterwards. The cache still
        // names the empty file, so only the length check makes the retry re-read it and
        // refuse version 1 instead of writing it a second time.
        let mut file = OpenOptions::new().append(true).open(store.path(&id))?;
        file.write_all(format!("{}\n", line(&first(&id))).as_bytes())?;
        file.sync_data()?;
        drop(file);
        let duplicate = store.append(&id, Version::ZERO, &[first(&id)]);
        assert!(
            matches!(
                duplicate,
                Err(StoreError::Conflict { expected, actual })
                    if expected == Version::ZERO && actual == Version::new(1)
            ),
            "the record it never saw must be refused: {duplicate:?}"
        );
        assert_eq!(store.load(&id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn every_event_variant_round_trips_through_the_line_format() {
        let events = [
            JobEvent::Submitted {
                input: b"in".to_vec(),
                max_attempts: NonZeroU32::new(3).expect("literal is nonzero"),
            },
            JobEvent::Started { attempt: 2 },
            JobEvent::RetryScheduled {
                attempt: 3,
                reason: "flaky".to_owned(),
            },
            JobEvent::Succeeded {
                output: b"out".to_vec(),
            },
            JobEvent::Failed {
                reason: "broken".to_owned(),
            },
            JobEvent::Cancelled {
                reason: "stop".to_owned(),
            },
        ];
        let id = id(22);
        for event in events {
            let record = at(&id, 1, event);
            let encoded = line(&record);
            assert!(!encoded.contains('\n'));
            let decoded: CloudEvent<JobEvent> =
                serde_json::from_str(&encoded).expect("record decodes");
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn keys_ignores_an_entry_that_is_not_a_stream_file() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(33);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        fs::write(store.root.join("notes.txt"), b"not a stream")?;
        fs::create_dir(store.root.join("archive.jsonl"))?;
        assert_eq!(store.keys()?, vec![id]);
        Ok(())
    }

    #[test]
    fn keys_skips_a_stream_file_whose_stem_is_not_a_key() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(33);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        fs::write(store.root.join("not-a-uuid.jsonl"), b"")?;
        assert_eq!(store.keys()?, vec![id]);
        Ok(())
    }

    #[test]
    fn keys_skips_a_stem_that_is_not_canonical() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(34);
        store.append(&id, Version::ZERO, &[first(&id)])?;
        let uppercase = id.as_str().to_uppercase();
        fs::write(store.root.join(format!("{uppercase}.jsonl")), b"")?;
        assert_eq!(store.keys()?, vec![id]);
        Ok(())
    }
}

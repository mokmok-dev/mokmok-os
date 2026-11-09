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
//! One store instance owns a root. The version cache lives in the instance, so
//! opening the same root twice gives two writers with separate caches. One
//! writer can then append a duplicate version after the other has moved on.
//! Callers should share one `Arc<JsonlEventStore>` across every actor system over
//! that root.
//!
//! Correctness across processes is not claimed; the logical fence is the
//! store's version check.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, StoreError, StreamKey, Version,
    canonical_keys, check_append, complete_prefix, encode_records, object_name, parse_records,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct JsonlEventStore<K, E> {
    root: PathBuf,
    state: Mutex<Locked<K>>,
    event: PhantomData<fn() -> E>,
}

#[derive(Debug)]
struct Locked<K> {
    versions: HashMap<K, Version>,
}

impl<K, E> JsonlEventStore<K, E> {
    /// Opens or creates the root directory. No stream is read until it is
    /// touched. A stream's file is named `<key>.jsonl`, where `<key>` is
    /// [`StreamKey::as_str`] (a canonical lowercase UUID for `JobId`).
    ///
    /// One instance owns a root. The version cache lives in the instance, so
    /// opening the same root twice gives two writers with separate caches. One
    /// writer can then append a duplicate version after the other has moved on.
    /// Share one `Arc<JsonlEventStore>` across every actor system instead.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the root cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| error_at(&root, &error))?;
        Ok(Self {
            root,
            state: Mutex::new(Locked {
                versions: HashMap::new(),
            }),
            event: PhantomData,
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Locked<K>>, StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Io("event store lock poisoned".to_owned()))
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
) -> Result<(Version, File), StoreError>
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
    Ok((version, file))
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
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error_at(&path, &error)),
        };
        let (_, events) = parse_records(
            &path.display().to_string(),
            key.as_str(),
            complete_prefix(&bytes),
        )?;
        Ok(events)
    }

    #[allow(clippy::significant_drop_tightening)] // The guard is this stream's single-writer fence.
    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[CloudEvent<E>],
    ) -> Result<Version, StoreError> {
        let path = self.path(key);
        let mut state = self.lock()?;
        if events.is_empty() {
            let current = match state.versions.get(key) {
                Some(version) => *version,
                None => match fs::read(&path) {
                    Ok(bytes) => {
                        parse_records::<E>(
                            &path.display().to_string(),
                            key.as_str(),
                            complete_prefix(&bytes),
                        )?
                        .0
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Version::ZERO,
                    Err(error) => return Err(error_at(&path, &error)),
                },
            };
            if current != expected {
                return Err(StoreError::Conflict {
                    expected,
                    actual: current,
                });
            }
            return Ok(current);
        }
        let (current, mut file) = if let Some(version) = state.versions.get(key) {
            if let Ok(file) = OpenOptions::new().append(true).open(&path) {
                (*version, file)
            } else {
                // A missing file is the common case; every other error repeats in `ensure`.
                let (version, file) = ensure::<E>(&path, key.as_str())?;
                state.versions.insert(key.clone(), version);
                (version, file)
            }
        } else {
            let (version, file) = ensure::<E>(&path, key.as_str())?;
            state.versions.insert(key.clone(), version);
            (version, file)
        };
        if current != expected {
            return Err(StoreError::Conflict {
                expected,
                actual: current,
            });
        }
        let last = check_append(key.as_str(), expected, events)?;
        let mut buffer = Vec::new();
        encode_records(events, &mut buffer)?;
        file.write_all(&buffer)
            .map_err(|error| error_at(&path, &error))?;
        file.sync_data().map_err(|error| error_at(&path, &error))?;
        state.versions.insert(key.clone(), last);
        Ok(last)
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

fn error_at(
    path: &Path,
    error: &std::io::Error,
) -> StoreError {
    StoreError::Io(format!("{}: {error}", path.display()))
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
    fn a_missing_stream_loads_empty() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(7);
        assert_eq!(store.load(&id)?.len(), 0);
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
    fn a_stale_append_is_refused_without_writing() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(9);
        store.append(&id, Version::ZERO, &[first(&id)])?;
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
        assert_eq!(store.load(&id)?.len(), 1);
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
    fn append_refuses_a_record_from_another_stream() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let other = id(17);
        let id = id(16);
        let error = store
            .append(&id, Version::ZERO, &[first(&other)])
            .expect_err("a record from another stream is refused");
        assert!(matches!(error, StoreError::Invalid(_)));
        assert_eq!(store.load(&id)?.len(), 0);
        Ok(())
    }

    #[test]
    fn append_refuses_a_record_that_skips_a_version() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(18);
        let error = store
            .append(&id, Version::ZERO, &[at(&id, 2, submitted())])
            .expect_err("a skipped version is refused");
        assert!(matches!(error, StoreError::Invalid(_)));
        assert_eq!(store.load(&id)?.len(), 0);
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
    fn keys_of_an_empty_root_is_empty() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        assert_eq!(store.keys()?, Vec::<JobId>::new());
        Ok(())
    }

    #[test]
    fn keys_lists_every_stream_in_key_order() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let low = id(31);
        let high = id(32);
        store.append(&high, Version::ZERO, &[first(&high)])?;
        store.append(&low, Version::ZERO, &[first(&low)])?;
        assert_eq!(store.keys()?, vec![low, high]);
        Ok(())
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

//! JSONL platform adapter for `mokmok-jobs`: one append-only file per stream.
//!
//! The file stem is the stream key. A `JobId` is a `UUIDv7`, so the file name is
//! the id itself.
//!
//! Each line is one `CloudEvents` 1.0 JSON record from `mokmok-jobs-core`. The file
//! holds no header and nothing else, so the same record shape is what the in-memory
//! adapter keeps.
//!
//! One store instance owns a root. The version cache lives in the instance, so
//! opening the same root twice gives two writers with separate caches. One
//! writer can then append a duplicate version after the other has moved on.
//! Callers should share one `Arc<JsonlEventStore>` across `JobSystem`s.
//!
//! Correctness across processes is not claimed; the logical fence is the
//! store's version check.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mokmok_jobs_core::{CloudEvent, EventStore, StoreError, StreamKey, Version, check_append};
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
    /// touched. A stream's file is named `<key>.jsonl`; a `JobId` key is a
    /// canonical lowercase UUID.
    ///
    /// One instance owns a root. The version cache lives in the instance, so
    /// opening the same root twice gives two writers with separate caches. One
    /// writer can then append a duplicate version after the other has moved on.
    /// Share one `Arc<JsonlEventStore>` across systems instead.
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
        self.root.join(format!("{}.jsonl", key.as_str()))
    }
}

fn ensure<E>(
    path: &Path,
    subject: &str,
) -> Result<(Version, File), StoreError>
where
    E: DeserializeOwned,
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
    let (version, _) = parse_stream::<E>(path, &bytes[..committed], subject)?;
    Ok((version, file))
}

impl<K, E> EventStore<K, E> for JsonlEventStore<K, E>
where
    K: StreamKey,
    E: Serialize + DeserializeOwned,
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
        let (_, events) = parse_stream(&path, complete_prefix(&bytes), key.as_str())?;
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
                    Ok(bytes) => parse_stream::<E>(&path, complete_prefix(&bytes), key.as_str())?.0,
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
        for event in events {
            serde_json::to_writer(&mut buffer, event)
                .map_err(|error| StoreError::Io(error.to_string()))?;
            buffer.push(b'\n');
        }
        file.write_all(&buffer)
            .map_err(|error| error_at(&path, &error))?;
        file.sync_data().map_err(|error| error_at(&path, &error))?;
        state.versions.insert(key.clone(), last);
        Ok(last)
    }
}

fn parse_stream<E>(
    path: &Path,
    bytes: &[u8],
    subject: &str,
) -> Result<(Version, Vec<CloudEvent<E>>), StoreError>
where
    E: DeserializeOwned,
{
    if bytes.is_empty() {
        return Ok((Version::ZERO, Vec::new()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| StoreError::Corrupt(format!("{}: {error}", path.display())))?;
    let mut version = Version::ZERO;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let event: CloudEvent<E> =
            serde_json::from_str(line).map_err(|error| corrupt(path, &error))?;
        let expected = version.next();
        if event.subject != subject {
            return Err(StoreError::Corrupt(format!(
                "{}:{}: record names stream {} instead of {subject}",
                path.display(),
                index + 1,
                event.subject
            )));
        }
        if event.version != expected {
            return Err(StoreError::Corrupt(format!(
                "{}:{}: expected version {}",
                path.display(),
                index + 1,
                expected.get()
            )));
        }
        version = event.version;
        events.push(event);
    }
    Ok((version, events))
}

fn complete_prefix(bytes: &[u8]) -> &[u8] {
    match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => &bytes[..=index],
        None => &[],
    }
}

fn corrupt(
    path: &Path,
    error: &serde_json::Error,
) -> StoreError {
    StoreError::Corrupt(format!("{}: {error}", path.display()))
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

    use mokmok_jobs_core::{CloudEvent, EventType, JobEvent, JobId};
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
}

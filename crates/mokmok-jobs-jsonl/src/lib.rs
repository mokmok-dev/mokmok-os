//! JSONL platform adapter for `mokmok-jobs`: one append-only file per stream.
//!
//! The file stem is the stream key. A `JobId` is a `UUIDv7`, so the file name is
//! the id itself.
//!
//! Each file starts with a header line naming its format, stream key, and
//! creation time.
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

use mokmok_jobs_core::{Envelope, EventStore, StoreError, StreamKey, Version};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const FORMAT: u32 = 1;

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

#[derive(Serialize, Deserialize)]
struct Header<K> {
    format: u32,
    stream: K,
    created_at: u64,
}

#[derive(Serialize)]
struct StoredEnvelope<'a, K, E> {
    stream: &'a K,
    version: Version,
    event: &'a E,
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

fn ensure<K, E>(
    path: &Path,
    key: &K,
) -> Result<(Version, bool, File), StoreError>
where
    K: StreamKey + DeserializeOwned,
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
    let (version, _) = parse_stream::<K, E>(path, &bytes[..committed], key)?;
    Ok((version, committed == 0, file))
}

impl<K, E> EventStore<K, E> for JsonlEventStore<K, E>
where
    K: StreamKey + Serialize + DeserializeOwned,
    E: Serialize + DeserializeOwned,
{
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<Envelope<K, E>>, StoreError> {
        let path = self.path(key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error_at(&path, &error)),
        };
        let (_, envelopes) = parse_stream(&path, complete_prefix(&bytes), key)?;
        Ok(envelopes)
    }

    #[allow(clippy::significant_drop_tightening)] // The guard is this stream's single-writer fence.
    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[E],
    ) -> Result<Version, StoreError> {
        let path = self.path(key);
        let mut state = self.lock()?;
        if events.is_empty() {
            let current = match state.versions.get(key) {
                Some(version) => *version,
                None => match fs::read(&path) {
                    Ok(bytes) => parse_stream::<K, E>(&path, complete_prefix(&bytes), key)?.0,
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
        let (current, needs_header, mut file) = if let Some(version) = state.versions.get(key) {
            if let Ok(file) = OpenOptions::new().append(true).open(&path) {
                (*version, false, file)
            } else {
                // A missing file is the common case; every other error repeats in `ensure`.
                let (version, needs_header, file) = ensure::<K, E>(&path, key)?;
                state.versions.insert(key.clone(), version);
                (version, needs_header, file)
            }
        } else {
            let (version, needs_header, file) = ensure::<K, E>(&path, key)?;
            state.versions.insert(key.clone(), version);
            (version, needs_header, file)
        };
        if current != expected {
            return Err(StoreError::Conflict {
                expected,
                actual: current,
            });
        }
        let mut buffer = Vec::new();
        if needs_header {
            let header = Header {
                format: FORMAT,
                stream: key.clone(),
                created_at: now_millis(),
            };
            serde_json::to_writer(&mut buffer, &header)
                .map_err(|error| StoreError::Io(error.to_string()))?;
            buffer.push(b'\n');
        }
        let mut version = current;
        for event in events {
            version = version.next();
            let envelope = StoredEnvelope {
                stream: key,
                version,
                event,
            };
            serde_json::to_writer(&mut buffer, &envelope)
                .map_err(|error| StoreError::Io(error.to_string()))?;
            buffer.push(b'\n');
        }
        file.write_all(&buffer)
            .map_err(|error| error_at(&path, &error))?;
        file.sync_data().map_err(|error| error_at(&path, &error))?;
        state.versions.insert(key.clone(), version);
        Ok(version)
    }
}

fn parse_stream<K, E>(
    path: &Path,
    bytes: &[u8],
    key: &K,
) -> Result<(Version, Vec<Envelope<K, E>>), StoreError>
where
    K: StreamKey + DeserializeOwned,
    E: DeserializeOwned,
{
    if bytes.is_empty() {
        return Ok((Version::ZERO, Vec::new()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| StoreError::Corrupt(format!("{}: {error}", path.display())))?;
    let mut lines = text.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| StoreError::Corrupt(format!("{}: missing header", path.display())))?;
    let header: Header<K> =
        serde_json::from_str(header_line).map_err(|error| corrupt(path, &error))?;
    if header.format != FORMAT {
        return Err(StoreError::Corrupt(format!(
            "{}: unsupported format {}",
            path.display(),
            header.format
        )));
    }
    if header.stream != *key {
        return Err(StoreError::Corrupt(format!(
            "{}: header names a different stream",
            path.display()
        )));
    }
    let mut version = Version::ZERO;
    let mut envelopes = Vec::new();
    for (index, line) in lines.enumerate() {
        let envelope: Envelope<K, E> =
            serde_json::from_str(line).map_err(|error| corrupt(path, &error))?;
        let expected = version.next();
        if envelope.stream != *key || envelope.version != expected {
            return Err(StoreError::Corrupt(format!(
                "{}:{}: expected version {}",
                path.display(),
                index + 2,
                expected.get()
            )));
        }
        version = envelope.version;
        envelopes.push(envelope);
    }
    Ok((version, envelopes))
}

fn complete_prefix(bytes: &[u8]) -> &[u8] {
    match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => &bytes[..=index],
        None => &[],
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
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

    use mokmok_jobs_core::JobId;
    use serde::{Deserialize, Serialize};

    use super::*;

    fn id(n: u128) -> JobId {
        JobId::from_u128(n)
    }

    fn submitted() -> mokmok_jobs_core::JobEvent {
        mokmok_jobs_core::JobEvent::Submitted {
            input: b"in".to_vec(),
            max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
        }
    }

    fn store(directory: &tempfile::TempDir) -> JsonlEventStore<JobId, mokmok_jobs_core::JobEvent> {
        JsonlEventStore::open(directory.path().join("jobs")).expect("store opens")
    }

    #[test]
    fn open_creates_missing_parent_directories() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("nested").join("deeper");
        let store = JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(&root)?;
        let id = id(1);
        store.append(&id, Version::ZERO, &[submitted()])?;
        assert_eq!(store.load(&id)?.len(), 1);
        assert!(root.is_dir());
        Ok(())
    }

    #[test]
    fn streams_live_in_separate_files() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let first = id(1);
        let second = id(2);
        store.append(&first, Version::ZERO, &[submitted()])?;
        store.append(
            &second,
            Version::ZERO,
            &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
        )?;
        assert_ne!(store.path(&first), store.path(&second));
        assert_eq!(store.load(&first)?.len(), 1);
        assert_eq!(store.load(&second)?.len(), 1);
        assert_ne!(store.load(&first)?[0].event, store.load(&second)?[0].event);
        Ok(())
    }

    #[test]
    fn reopening_twice_keeps_every_committed_record() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let id = id(3);
        {
            let store = JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(&root)?;
            store.append(&id, Version::ZERO, &[submitted()])?;
            store.append(
                &id,
                Version::new(1),
                &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
            )?;
        }
        {
            drop(JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(
                &root,
            )?);
        }
        let store = JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(&root)?;
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn opening_an_existing_stream_keeps_its_version() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("jobs");
        let id = id(4);
        {
            let store = JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(&root)?;
            store.append(&id, Version::ZERO, &[submitted()])?;
        }
        let store = JsonlEventStore::<JobId, mokmok_jobs_core::JobEvent>::open(&root)?;
        store.append(
            &id,
            Version::new(1),
            &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
        )?;
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn torn_tail_is_discarded_before_the_next_append() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let writer = store(&directory);
        let id = id(5);
        writer.append(&id, Version::ZERO, &[submitted()])?;
        let mut file = OpenOptions::new().append(true).open(writer.path(&id))?;
        file.write_all(format!("{{\"stream\":\"{}\",", id.as_str()).as_bytes())?;
        drop(file);
        let store = store(&directory);
        assert_eq!(store.load(&id)?.len(), 1);
        let version = store.append(
            &id,
            Version::new(1),
            &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
        )?;
        assert_eq!(version, Version::new(2));
        assert_eq!(store.load(&id)?.len(), 2);
        Ok(())
    }

    #[test]
    fn a_torn_header_is_replaced_on_append() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(6);
        fs::write(store.path(&id), b"{\"format\":1")?;
        assert_eq!(store.load(&id)?.len(), 0);
        store.append(&id, Version::ZERO, &[submitted()])?;
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
        store.append(&id, Version::ZERO, &[submitted()])?;
        assert_eq!(store.append(&id, Version::new(1), &[])?, Version::new(1));
        Ok(())
    }

    #[test]
    fn a_stale_append_is_refused_without_writing() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(9);
        store.append(&id, Version::ZERO, &[submitted()])?;
        let error = store
            .append(
                &id,
                Version::ZERO,
                &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
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
    fn a_header_for_another_stream_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let other = id(11);
        let id = id(10);
        fs::write(
            store.path(&id),
            format!(
                "{{\"format\":1,\"stream\":\"{}\",\"created_at\":1}}\n",
                other.as_str()
            ),
        )?;
        let error = store.load(&id).expect_err("mismatched header is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn an_unsupported_format_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(12);
        fs::write(
            store.path(&id),
            format!(
                "{{\"format\":2,\"stream\":\"{}\",\"created_at\":1}}\n",
                id.as_str()
            ),
        )?;
        let error = store.load(&id).expect_err("future format is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn a_malformed_complete_line_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(13);
        fs::write(
            store.path(&id),
            format!(
                "{{\"format\":1,\"stream\":\"{}\",\"created_at\":1}}\nnot json\n",
                id.as_str()
            ),
        )?;
        let error = store.load(&id).expect_err("malformed line is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn the_header_records_the_format_stream_and_creation_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(14);
        let before = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )?;
        store.append(&id, Version::ZERO, &[submitted()])?;
        let text = fs::read_to_string(store.path(&id))?;
        let header: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("header line"))?;
        assert_eq!(header["format"], 1);
        assert_eq!(header["stream"], id.as_str());
        let created = header["created_at"]
            .as_u64()
            .expect("created_at is a number");
        assert!(created >= before);
        Ok(())
    }

    #[test]
    fn the_adapter_is_generic_over_key_and_event() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

        let directory = tempfile::tempdir()?;
        let store = JsonlEventStore::<NoteId, Note>::open(directory.path().join("notes"))?;
        let id = NoteId("note-1".to_owned());
        store.append(
            &id,
            Version::ZERO,
            &[Note {
                text: "hello".to_owned(),
            }],
        )?;
        assert_eq!(store.load(&id)?[0].event.text, "hello");
        Ok(())
    }

    #[test]
    fn a_read_error_that_is_not_missing_is_reported() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let id = id(15);
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
        let id = id(16);
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
        let id = id(17);
        store.append(&id, Version::ZERO, &[submitted()])?;
        fs::remove_file(store.path(&id))?;
        let error = store
            .append(
                &id,
                Version::new(1),
                &[mokmok_jobs_core::JobEvent::Started { attempt: 1 }],
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
    fn an_envelope_for_another_stream_is_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = store(&directory);
        let other = id(19);
        let id = id(18);
        fs::write(
            store.path(&id),
            format!(
                "{{\"format\":1,\"stream\":\"{}\",\"created_at\":1}}\n{{\"stream\":\"{}\",\"version\":1,\"event\":{{\"kind\":\"started\",\"attempt\":1}}}}\n",
                id.as_str(),
                other.as_str()
            ),
        )?;
        let error = store
            .load(&id)
            .expect_err("an envelope from another stream is refused");
        assert!(matches!(error, StoreError::Corrupt(_)));
        Ok(())
    }

    #[test]
    fn every_event_variant_round_trips_through_the_line_format() {
        use mokmok_jobs_core::{Envelope, JobEvent};

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
        for event in events {
            let envelope = Envelope {
                stream: id(20),
                version: Version::new(1),
                event: event.clone(),
            };
            let line = serde_json::to_string(&envelope).expect("event encodes");
            assert!(!line.contains('\n'));
            let decoded: Envelope<JobId, JobEvent> =
                serde_json::from_str(&line).expect("event decodes");
            assert_eq!(decoded, envelope);
        }
    }
}

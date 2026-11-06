//! JSONL platform adapter for `mokmok-jobs`: one event per line in one append-only
//! file.
//!
//! One writer process per file. Correctness across processes is not claimed; the
//! logical fence is the store's version check.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mokmok_jobs_core::{Envelope, EventStore, JobEvent, JobId, StoreError, Version};

#[derive(Debug)]
pub struct JsonlEventStore {
    path: PathBuf,
    state: Mutex<Locked>,
}

#[derive(Debug)]
struct Locked {
    file: File,
    versions: HashMap<JobId, Version>,
}

impl JsonlEventStore {
    /// Opens or creates the log, discarding a torn tail left by an interrupted append.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the file cannot be opened and
    /// [`StoreError::Corrupt`] when a complete line is unreadable or a stream's
    /// versions are not contiguous from 1.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| error_at(path.as_path(), &error))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|error| error_at(path.as_path(), &error))?;
        let versions = scan(path.as_path(), &mut file)?;
        Ok(Self {
            path,
            state: Mutex::new(Locked { file, versions }),
        })
    }
}

impl EventStore for JsonlEventStore {
    fn load(
        &self,
        id: &JobId,
    ) -> Result<Vec<Envelope>, StoreError> {
        let mut bytes = Vec::new();
        {
            let mut state = self.lock()?;
            state
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|error| error_at(self.path.as_path(), &error))?;
            state
                .file
                .read_to_end(&mut bytes)
                .map_err(|error| error_at(self.path.as_path(), &error))?;
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| StoreError::Corrupt(format!("{}: {error}", self.path.display())))?;
        let mut envelopes = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let envelope = parse_line(self.path.as_path(), index, line)?;
            if envelope.job_id == *id {
                envelopes.push(envelope);
            }
        }
        Ok(envelopes)
    }

    fn append(
        &self,
        id: &JobId,
        expected: Version,
        events: &[JobEvent],
    ) -> Result<Version, StoreError> {
        let mut state = self.lock()?;
        let current = state.versions.get(id).copied().unwrap_or(Version::ZERO);
        if current != expected {
            return Err(StoreError::Conflict {
                expected,
                actual: current,
            });
        }
        if events.is_empty() {
            return Ok(current);
        }
        let mut version = current;
        let mut buffer = Vec::new();
        for event in events {
            version = version.next();
            let envelope = Envelope {
                job_id: id.clone(),
                version,
                event: event.clone(),
            };
            serde_json::to_writer(&mut buffer, &envelope)
                .map_err(|error| StoreError::Io(format!("{}: {error}", self.path.display())))?;
            buffer.push(b'\n');
        }
        state
            .file
            .write_all(&buffer)
            .map_err(|error| error_at(self.path.as_path(), &error))?;
        state
            .file
            .sync_data()
            .map_err(|error| error_at(self.path.as_path(), &error))?;
        state.versions.insert(id.clone(), version);
        drop(state);
        Ok(version)
    }
}

impl JsonlEventStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Locked>, StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Io(format!("{}: lock poisoned", self.path.display())))
    }
}

fn scan(
    path: &Path,
    file: &mut File,
) -> Result<HashMap<JobId, Version>, StoreError> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error_at(path, &error))?;
    // A committed record always ends in a newline, so anything after the last one is a
    // torn append from an interrupted write.
    let committed = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if committed < bytes.len() {
        let length = u64::try_from(committed).map_err(|error| StoreError::Io(error.to_string()))?;
        file.set_len(length)
            .map_err(|error| error_at(path, &error))?;
        bytes.truncate(committed);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| StoreError::Corrupt(format!("{}: {error}", path.display())))?;
    let mut versions: HashMap<JobId, Version> = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        let envelope = parse_line(path, index, line)?;
        let expected = versions
            .get(&envelope.job_id)
            .map_or(Version::new(1), |version| version.next());
        if envelope.version != expected {
            return Err(StoreError::Corrupt(format!(
                "{}:{}: job {} expected version {} but found {}",
                path.display(),
                index + 1,
                envelope.job_id.as_str(),
                expected.get(),
                envelope.version.get()
            )));
        }
        versions.insert(envelope.job_id, envelope.version);
    }
    Ok(versions)
}

fn parse_line(
    path: &Path,
    index: usize,
    line: &str,
) -> Result<Envelope, StoreError> {
    serde_json::from_str(line)
        .map_err(|error| StoreError::Corrupt(format!("{}:{}: {error}", path.display(), index + 1)))
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

    use super::*;

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
        for event in events {
            let envelope = Envelope {
                job_id: JobId::parse("encoded").expect("literal is valid"),
                version: Version::new(1),
                event: event.clone(),
            };
            let line = serde_json::to_string(&envelope).expect("event encodes");
            assert!(!line.contains('\n'));
            let decoded: Envelope = serde_json::from_str(&line).expect("event decodes");
            assert_eq!(decoded, envelope);
        }
    }
}

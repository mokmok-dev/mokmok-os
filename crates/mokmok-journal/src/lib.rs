//! The per-task execution journal: compact operational telemetry the runner writes
//! beside the domain event store.
//!
//! A task's domain stream carries only the facts that decide its state. The journal
//! carries what the agent said and did: message and thought text, tool calls, and
//! permission decisions. It is not replayed into the domain state, so losing an old
//! segment loses display history and never the task's business state.
//!
//! Every string field of a record is clipped to [`MAX_RECORD_BYTES`] before it is
//! stored or returned, so one huge ACP chunk or tool title cannot escape the per-task
//! bound, including a record an earlier build stored oversized. The journal holds
//! unredacted agent output, including tool titles that may carry a command line; on
//! Unix the root, task directories, and segments are created private (`0700`/`0600`),
//! and an existing root keeps its own mode.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The default segment budget of a [`FileJournal`].
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The default number of segments a [`FileJournal`] keeps.
pub const DEFAULT_KEEP_SEGMENTS: usize = 8;

/// The default age after which a rotated segment is dropped.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_hours(7 * 24);

/// The largest a journal record's string fields may be, per field, after clipping.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

/// One journaled step of a task's transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEvent {
    Message { text: String },
    Thought { text: String },
    ToolCall { title: String, status: String },
    Permission { title: String, status: String },
}

impl JournalEvent {
    fn needs_clip(&self) -> bool {
        let over = |text: &str| text.len() > MAX_RECORD_BYTES;
        match self {
            Self::Message { text } | Self::Thought { text } => over(text),
            Self::ToolCall { title, status } | Self::Permission { title, status } => {
                over(title) || over(status)
            },
        }
    }

    /// Returns this event with every string field clipped to [`MAX_RECORD_BYTES`].
    #[must_use]
    pub fn clipped(&self) -> Self {
        let clip = |text: &str| text[..text.floor_char_boundary(MAX_RECORD_BYTES)].to_owned();
        match self {
            Self::Message { text } => Self::Message { text: clip(text) },
            Self::Thought { text } => Self::Thought { text: clip(text) },
            Self::ToolCall { title, status } => Self::ToolCall {
                title: clip(title),
                status: clip(status),
            },
            Self::Permission { title, status } => Self::Permission {
                title: clip(title),
                status: clip(status),
            },
        }
    }
}

/// One stamped journal record, as it is stored and served.
///
/// The flattened event keeps the field names the control plane's UI already reads
/// (`kind`, `text`, `title`, `status`), with `seq` and `time` beside them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub time: String,
    #[serde(flatten)]
    pub event: JournalEvent,
}

/// The newest entries of a task's journal, with the journal's total entry count.
///
/// `total` is the count at the snapshot the read started from, raised to at least
/// `entries.len()`; the entries are the newest at read time, so a concurrent append or
/// prune can still leave the two disagreeing above that floor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalTail {
    pub entries: Vec<JournalEntry>,
    pub total: u64,
}

/// How a [`FileJournal`] bounds its storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalConfig {
    /// The size at which the active segment rotates, unless a single record is larger.
    pub max_bytes: u64,
    /// How many newest segments a task keeps, counting the active one.
    pub keep_segments: usize,
    /// How old a rotated segment may be before it is dropped.
    ///
    /// [`Duration::ZERO`] disables age pruning.
    pub max_age: Duration,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            keep_segments: DEFAULT_KEEP_SEGMENTS,
            max_age: DEFAULT_MAX_AGE,
        }
    }
}

/// Why a journal append, tail, or count failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    InvalidKey(String),
    /// A listed segment no longer exists; only a retry can tell this from a real loss.
    Missing(String),
    Io(String),
}

impl fmt::Display for JournalError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => write!(
                formatter,
                "journal key {key:?} must be a single [a-z0-9-]+ path component"
            ),
            Self::Missing(path) => write!(formatter, "journal segment {path} is missing"),
            Self::Io(message) => write!(formatter, "journal i/o: {message}"),
        }
    }
}

impl std::error::Error for JournalError {}

/// The append-only, bounded transcript store a runner writes through.
pub trait Journal: Send + Sync {
    /// Appends `events` to `task`'s journal and returns them stamped with their
    /// sequence number and time, so a live feed can publish the same records.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::InvalidKey`] when `task` is not a safe path component,
    /// and [`JournalError::Io`] when the journal cannot be written.
    fn append(
        &self,
        task: &str,
        events: &[JournalEvent],
    ) -> Result<Vec<JournalEntry>, JournalError>;

    /// Returns up to `limit` newest entries of `task` in ascending sequence order,
    /// together with the task's total entry count.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::InvalidKey`] when `task` is not a safe path component,
    /// and [`JournalError::Missing`] or [`JournalError::Io`] when a segment cannot be
    /// read.
    fn tail(
        &self,
        task: &str,
        limit: usize,
    ) -> Result<JournalTail, JournalError>;

    /// Returns how many entries `task`'s journal holds.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::InvalidKey`] when `task` is not a safe path component,
    /// and [`JournalError::Missing`] or [`JournalError::Io`] when a segment cannot be
    /// read.
    fn count(
        &self,
        task: &str,
    ) -> Result<u64, JournalError>;
}

/// A journal of one JSONL segment per generation under `<root>/<task>/`.
///
/// One instance owns a root, like the domain store: the sequence cursor lives in the
/// instance. Reopening a root finds the next sequence from the newest stored record,
/// so a restart continues the numbering instead of repeating it.
#[derive(Debug)]
pub struct FileJournal {
    root: PathBuf,
    config: JournalConfig,
    state: Mutex<Locked>,
}

#[derive(Debug)]
struct Locked {
    cursors: HashMap<String, Cursor>,
}

#[derive(Clone, Copy, Debug)]
struct Cursor {
    next_seq: u64,
    generation: u32,
    bytes: u64,
    count: u64,
    // Whether `count` covers every stored record. Warming past an unreadable segment
    // leaves a count that only covers the readable ones, so a prune that removes anything
    // must have the count rebuilt rather than adjusted.
    count_exact: bool,
}

// What a prune took away: the segments and, only when the caller asked for them, the
// entries they held.
#[derive(Debug, Default, PartialEq, Eq)]
struct Removed {
    segments: u64,
    entries: u64,
}

impl FileJournal {
    /// Opens or creates `root`, applies `config` to every append, and sweeps retention
    /// once so a task that stopped writing still loses its expired segments.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] when the root cannot be created. A task directory
    /// the sweep cannot read is skipped rather than failing startup.
    pub fn open(
        root: impl Into<PathBuf>,
        config: JournalConfig,
    ) -> Result<Self, JournalError> {
        let root = root.into();
        create_dir(&root)?;
        let journal = Self {
            root,
            config,
            state: Mutex::new(Locked {
                cursors: HashMap::new(),
            }),
        };
        journal.sweep();
        Ok(journal)
    }

    // Retention otherwise only runs on append, so a finished task would keep its old
    // segments forever. One unreadable task directory must not fail startup. Warming each
    // cursor here makes the first count a cache hit instead of a scan under the lock.
    fn sweep(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let mut cursors = HashMap::new();
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some(task) = name.to_str() else {
                continue;
            };
            let _ = self.prune(task, false);
            let Ok(segments) = self.segments(task) else {
                continue;
            };
            let Ok(cursor) = initialise_cursor(&segments) else {
                continue;
            };
            cursors.insert(task.to_owned(), cursor);
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.cursors = cursors;
    }

    fn segments(
        &self,
        task: &str,
    ) -> Result<Vec<(u32, PathBuf)>, JournalError> {
        let directory = self.root.join(task);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error_at(&directory, &error)),
        };
        let mut segments = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| error_at(&directory, &error))?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(stem) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let Ok(generation) = stem.parse::<u32>() else {
                continue;
            };
            if generation.to_string() != stem {
                continue;
            }
            if !entry
                .file_type()
                .map_err(|error| error_at(&path, &error))?
                .is_file()
            {
                continue;
            }
            segments.push((generation, path));
        }
        segments.sort_by_key(|(generation, _)| *generation);
        Ok(segments)
    }

    // The lock covers the listing and the cached count, not the entry reads.
    fn snapshot(
        &self,
        task: &str,
    ) -> Result<(Vec<(u32, PathBuf)>, u64), JournalError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let segments = self.segments(task)?;
        let total = if let Some(cursor) = state.cursors.get(task) {
            cursor.count
        } else {
            let cursor = initialise_cursor(&segments)?;
            let total = cursor.count;
            state.cursors.insert(task.to_owned(), cursor);
            total
        };
        drop(state);
        Ok((segments, total))
    }

    fn snapshot_tail(
        &self,
        task: &str,
        limit: usize,
    ) -> Result<JournalTail, JournalError> {
        let (segments, total) = self.snapshot(task)?;
        let entries = if limit == 0 {
            Vec::new()
        } else {
            tail_entries(&segments, limit)?
        };
        // The count is the snapshot's and the entries are read after it, so an append
        // between the two can leave more entries than the count; the detail response's
        // "N of M" must never read backwards.
        let total = total.max(u64::try_from(entries.len()).unwrap_or(u64::MAX));
        Ok(JournalTail { entries, total })
    }

    // A task's segment count and ages are both bounded, so the scan an append pays for
    // is a handful of directory entries. Returns the segments that went and, only when
    // `count_removed`, the entries they held: an exact cursor subtracts that count, while
    // an inexact one only needs to know that a removal left its lower bound stale. The
    // startup sweep passes `false` because nothing reads the count it would have to decode.
    fn prune(
        &self,
        task: &str,
        count_removed: bool,
    ) -> Result<Removed, JournalError> {
        let mut segments = self.segments(task)?;
        let active = segments.last().map_or(0, |(generation, _)| *generation);
        // A segment whose count is needed stays when it cannot be read: the append must
        // succeed, and the count stays right because the segment is still counted. One
        // whose count is not needed is removed unread, and the rebuild after a removal is
        // what repairs the cursor.
        let entries_of = |path: &Path| -> Option<u64> {
            if count_removed {
                segment_entries(path).ok()
            } else {
                Some(0)
            }
        };
        let mut removed = Removed::default();
        if !self.config.max_age.is_zero() {
            let mut kept = Vec::with_capacity(segments.len());
            for (generation, path) in segments {
                let expired = generation != active
                    && fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .is_ok_and(|modified| {
                            expired(modified, SystemTime::now(), self.config.max_age)
                        });
                if expired
                    && let Some(entries) = entries_of(&path)
                    && fs::remove_file(&path).is_ok()
                {
                    removed.segments = removed.segments.saturating_add(1);
                    removed.entries = removed.entries.saturating_add(entries);
                } else {
                    kept.push((generation, path));
                }
            }
            segments = kept;
        }
        let keep = self.config.keep_segments.max(1);
        // Walk the list once: a segment whose count is unavailable, or whose removal
        // fails, stays in it and is not retried, while the next candidate can still be
        // evicted.
        let mut index = 0;
        while segments.len() > keep && index < segments.len() {
            if segments[index].0 != active
                && let Some(entries) = entries_of(&segments[index].1)
                && fs::remove_file(&segments[index].1).is_ok()
            {
                removed.segments = removed.segments.saturating_add(1);
                removed.entries = removed.entries.saturating_add(entries);
                segments.remove(index);
                continue;
            }
            index = index.saturating_add(1);
        }
        Ok(removed)
    }

    fn append_locked(
        &self,
        task: &str,
        directory: &Path,
        events: &[JournalEvent],
        state: &mut Locked,
    ) -> Result<Vec<JournalEntry>, JournalError> {
        // The scan is lazy: a cached cursor costs no directory read, so the common
        // append pays only for `prune`.
        let mut cursor = if let Some(cursor) = state.cursors.get(task) {
            *cursor
        } else {
            let segments = self.segments(task)?;
            initialise_cursor(&segments)?
        };
        let active = (cursor.generation, cursor.bytes);
        // A crash can leave a record without its newline; a separator keeps the next
        // record from being glued to the torn one, where neither would parse.
        let separator = active.1 > 0 && !ends_with_newline(&segment_path(directory, active.0))?;
        if separator {
            cursor.bytes = cursor.bytes.saturating_add(1);
        }
        let time = timestamp(SystemTime::now());
        let mut entries = Vec::with_capacity(events.len());
        for (offset, event) in events.iter().enumerate() {
            entries.push(JournalEntry {
                seq: cursor
                    .next_seq
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX)),
                time: time.clone(),
                event: event.clone(),
            });
        }

        let mut batches: Vec<(u32, Vec<u8>)> = Vec::new();
        for entry in &entries {
            let mut line =
                serde_json::to_vec(entry).map_err(|error| JournalError::Io(error.to_string()))?;
            line.push(b'\n');
            let size = u64::try_from(line.len()).unwrap_or(u64::MAX);
            if cursor.bytes > 0 && cursor.bytes.saturating_add(size) > self.config.max_bytes {
                cursor.generation = cursor.generation.saturating_add(1);
                cursor.bytes = 0;
            }
            match batches.last_mut() {
                Some((generation, buffer)) if *generation == cursor.generation => {
                    buffer.extend_from_slice(&line);
                },
                _ => batches.push((cursor.generation, line)),
            }
            cursor.bytes = cursor.bytes.saturating_add(size);
        }

        for (generation, buffer) in &batches {
            let path = segment_path(directory, *generation);
            let mut file = open_append(&path).map_err(|error| error_at(&path, &error))?;
            if separator && *generation == active.0 {
                file.write_all(b"\n")
                    .map_err(|error| error_at(&path, &error))?;
            }
            file.write_all(buffer)
                .map_err(|error| error_at(&path, &error))?;
        }
        // The new entries are counted before pruning so a segment this append wrote and
        // prune then removes is subtracted exactly once.
        cursor.count = cursor
            .count
            .saturating_add(u64::try_from(entries.len()).unwrap_or(u64::MAX));
        let removed = self.prune(task, cursor.count_exact)?;
        if cursor.count_exact {
            cursor.count = cursor.count.saturating_sub(removed.entries);
        } else if removed.segments > 0 {
            // The lower bound never held the removed entries, so only a re-count of what
            // is left can repair it; with nothing removed, the lower bound plus this
            // batch is still right and the scan would be wasted.
            let segments = self.segments(task)?;
            let (count, count_exact) = count_segments(&segments);
            cursor.count = count;
            cursor.count_exact = count_exact;
        }
        cursor.next_seq = cursor
            .next_seq
            .saturating_add(u64::try_from(entries.len()).unwrap_or(u64::MAX));
        state.cursors.insert(task.to_owned(), cursor);
        Ok(entries)
    }
}

impl Journal for FileJournal {
    #[allow(clippy::significant_drop_tightening)] // The guard is this journal's single-writer fence.
    fn append(
        &self,
        task: &str,
        events: &[JournalEvent],
    ) -> Result<Vec<JournalEntry>, JournalError> {
        validate_key(task)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let directory = self.root.join(task);
        create_dir(&directory)?;
        let clipped: Vec<JournalEvent> = events.iter().map(JournalEvent::clipped).collect();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let result = self.append_locked(task, &directory, &clipped, &mut state);
        if result.is_err() {
            // A failed batch can leave bytes unaccounted for, so the next append must
            // re-derive the cursor from disk rather than reuse a sequence number.
            state.cursors.remove(task);
        }
        result
    }

    fn tail(
        &self,
        task: &str,
        limit: usize,
    ) -> Result<JournalTail, JournalError> {
        validate_key(task)?;
        // Only the snapshot is taken under the lock; the segment reads happen outside it,
        // so one large tail never blocks an agent's append.
        // A listed segment vanished between the snapshot and the read, so one fresh
        // snapshot can still answer; any other failure is real and retrying it would
        // re-read the whole readable prefix for nothing.
        match self.snapshot_tail(task, limit) {
            Err(JournalError::Missing(_)) => self.snapshot_tail(task, limit),
            result => result,
        }
    }

    fn count(
        &self,
        task: &str,
    ) -> Result<u64, JournalError> {
        validate_key(task)?;
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(cursor) = state.cursors.get(task) {
                return Ok(cursor.count);
            }
        }
        let (_, total) = self.snapshot(task)?;
        Ok(total)
    }
}

/// The shared in-memory double for the [`Journal`] port.
#[derive(Debug, Default)]
pub struct MemoryJournal {
    tasks: Mutex<HashMap<String, Vec<JournalEntry>>>,
}

impl MemoryJournal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Journal for MemoryJournal {
    #[allow(clippy::significant_drop_tightening)] // The guard covers the stamping and the append.
    fn append(
        &self,
        task: &str,
        events: &[JournalEvent],
    ) -> Result<Vec<JournalEntry>, JournalError> {
        validate_key(task)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let time = timestamp(SystemTime::now());
        let clipped: Vec<JournalEvent> = events.iter().map(JournalEvent::clipped).collect();
        let mut stamped = Vec::with_capacity(clipped.len());
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = tasks.entry(task.to_owned()).or_default();
        let next = entries
            .last()
            .map_or(1, |entry| entry.seq.saturating_add(1));
        for (offset, event) in clipped.iter().enumerate() {
            stamped.push(JournalEntry {
                seq: next.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX)),
                time: time.clone(),
                event: event.clone(),
            });
        }
        entries.extend(stamped.iter().cloned());
        Ok(stamped)
    }

    fn tail(
        &self,
        task: &str,
        limit: usize,
    ) -> Result<JournalTail, JournalError> {
        validate_key(task)?;
        let tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = tasks.get(task).map_or_else(Vec::new, |entries| {
            let start = entries.len().saturating_sub(limit);
            entries[start..].to_vec()
        });
        let total = tasks.get(task).map_or(0, |entries| {
            u64::try_from(entries.len()).unwrap_or(u64::MAX)
        });
        drop(tasks);
        Ok(JournalTail { entries, total })
    }

    fn count(
        &self,
        task: &str,
    ) -> Result<u64, JournalError> {
        validate_key(task)?;
        let tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        let count = tasks.get(task).map_or(0, |entries| {
            u64::try_from(entries.len()).unwrap_or(u64::MAX)
        });
        drop(tasks);
        Ok(count)
    }
}

// A journal key becomes a directory name under the root, so it has to be exactly one
// safe component: a caller cannot name a path that climbs out.
fn validate_key(task: &str) -> Result<(), JournalError> {
    let valid = !task.is_empty()
        && task
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(JournalError::InvalidKey(task.to_owned()))
    }
}

fn initialise_cursor(segments: &[(u32, PathBuf)]) -> Result<Cursor, JournalError> {
    // An unreadable older segment is skipped rather than failing the cursor: a cold
    // cursor has to warm so the task keeps appending after a restart, and the count then
    // covers the segments that are readable. The newest segment stays required because
    // `next_seq` and the append offset both come from it.
    let (older, newest) = segments.split_at(segments.len().saturating_sub(1));
    let (count, count_exact) = count_segments(older);
    let cursor = match newest.first() {
        Some((generation, path)) => Cursor {
            next_seq: last_seq(segments)?.map_or(1, |seq| seq.saturating_add(1)),
            generation: *generation,
            bytes: fs::metadata(path)
                .map_err(|error| error_at(path, &error))?
                .len(),
            count: count.saturating_add(segment_entries(path)?),
            count_exact,
        },
        None => Cursor {
            next_seq: 1,
            generation: 0,
            bytes: 0,
            count: 0,
            count_exact: true,
        },
    };
    Ok(cursor)
}

// Counts the decodable records of `segments`, reporting whether every segment could be
// read so a caller knows a short count is short for a reason.
fn count_segments(segments: &[(u32, PathBuf)]) -> (u64, bool) {
    let mut count = 0_u64;
    let mut exact = true;
    for (_, path) in segments {
        match segment_entries(path) {
            Ok(entries) => count = count.saturating_add(entries),
            Err(_) => exact = false,
        }
    }
    (count, exact)
}

fn expired(
    modified: SystemTime,
    now: SystemTime,
    max_age: Duration,
) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > max_age)
}

fn segment_entries(path: &Path) -> Result<u64, JournalError> {
    let bytes = read_lines(path)?;
    Ok(u64::try_from(
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| decode(line).is_some())
            .count(),
    )
    .unwrap_or(u64::MAX))
}

fn tail_entries(
    segments: &[(u32, PathBuf)],
    limit: usize,
) -> Result<Vec<JournalEntry>, JournalError> {
    let mut entries = Vec::new();
    for (_, path) in segments.iter().rev() {
        for line in read_lines(path)?.split(|byte| *byte == b'\n').rev() {
            if line.is_empty() {
                continue;
            }
            let Some(entry) = decode(line) else {
                continue;
            };
            entries.push(entry);
            if entries.len() == limit {
                break;
            }
        }
        if entries.len() == limit {
            break;
        }
    }
    entries.reverse();
    Ok(entries)
}

// A listed segment that has vanished was removed by something other than this
// journal (whose appends hold the same lock), so its absence is a real read error
// rather than an empty transcript; it is a distinct variant only so a tail can retry it.
fn read_lines(path: &Path) -> Result<Vec<u8>, JournalError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(JournalError::Missing(path.display().to_string()))
        },
        Err(error) => Err(error_at(path, &error)),
    }
}

fn create_dir(path: &Path) -> Result<(), JournalError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| error_at(path, &error))
}

fn open_append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn last_seq(segments: &[(u32, PathBuf)]) -> Result<Option<u64>, JournalError> {
    for (_, path) in segments.iter().rev() {
        for line in read_lines(path)?.split(|byte| *byte == b'\n').rev() {
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = decode(line) {
                return Ok(Some(entry.seq));
            }
        }
    }
    Ok(None)
}

fn segment_path(
    directory: &Path,
    generation: u32,
) -> PathBuf {
    directory.join(format!("{generation}.jsonl"))
}

fn ends_with_newline(path: &Path) -> Result<bool, JournalError> {
    let mut file = File::open(path).map_err(|error| error_at(path, &error))?;
    if file.seek(SeekFrom::End(-1)).is_err() {
        return Ok(true);
    }
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)
        .map_err(|error| error_at(path, &error))?;
    Ok(byte[0] == b'\n')
}

// A torn or unparseable line is telemetry losing one display line; the task must not
// fail over it. Clipping here bounds a record an earlier build stored oversized, and
// the length check keeps the count path from rebuilding every small record.
fn decode(line: &[u8]) -> Option<JournalEntry> {
    let mut entry: JournalEntry = serde_json::from_str(std::str::from_utf8(line).ok()?).ok()?;
    if entry.event.needs_clip() {
        entry.event = entry.event.clipped();
    }
    Some(entry)
}

fn error_at(
    path: &Path,
    error: &std::io::Error,
) -> JournalError {
    JournalError::Io(format!("{}: {error}", path.display()))
}

/// Formats `time` as an RFC 3339 UTC stamp with milliseconds.
///
/// RFC 3339 needs a civil date, and the workspace carries no date crate for one format
/// string.
#[must_use]
pub fn timestamp(time: SystemTime) -> String {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let seconds = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    let days = seconds / 86_400;
    let clock = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = clock / 3_600;
    let minute = (clock % 3_600) / 60;
    let second = clock % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

// Howard Hinnant's days-to-civil conversion, with 1970-01-01 as day zero.
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::Path;

    use super::*;

    const TASK: &str = "00000000-0000-0000-0000-000000000001";

    fn message(text: &str) -> JournalEvent {
        JournalEvent::Message {
            text: text.to_owned(),
        }
    }

    fn empty_tail() -> JournalTail {
        JournalTail {
            entries: Vec::new(),
            total: 0,
        }
    }

    fn journal(
        directory: &tempfile::TempDir,
        config: JournalConfig,
    ) -> FileJournal {
        FileJournal::open(directory.path().join("journal"), config).expect("the journal opens")
    }

    fn rotate_every_entry() -> JournalConfig {
        JournalConfig {
            max_bytes: 1,
            ..JournalConfig::default()
        }
    }

    fn generations(journal: &FileJournal) -> Vec<u32> {
        journal
            .segments(TASK)
            .expect("the segments list")
            .into_iter()
            .map(|(generation, _)| generation)
            .collect()
    }

    fn cached_cursor(
        journal: &FileJournal,
        task: &str,
    ) -> Cursor {
        journal
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cursors
            .get(task)
            .copied()
            .expect("the cursor is cached")
    }

    fn write_entries(
        journal: &FileJournal,
        count: usize,
    ) {
        for index in 0..count {
            journal
                .append(TASK, &[message(&format!("m{index}"))])
                .expect("the append succeeds");
        }
    }

    fn line(entry: &JournalEntry) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(entry).expect("the entry encodes");
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn an_entry_flattens_its_kind_and_fields() {
        let entry = JournalEntry {
            seq: 7,
            time: "2026-11-08T06:19:08.123Z".to_owned(),
            event: JournalEvent::ToolCall {
                title: "read".to_owned(),
                status: "completed".to_owned(),
            },
        };
        let json = serde_json::to_value(&entry).expect("the entry encodes");
        assert_eq!(json["seq"], 7);
        assert_eq!(json["time"], "2026-11-08T06:19:08.123Z");
        assert_eq!(json["kind"], "tool_call");
        assert_eq!(json["title"], "read");
        assert_eq!(json["status"], "completed");
        let decoded: JournalEntry = serde_json::from_value(json).expect("the entry decodes");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn every_event_round_trips_through_json() {
        for event in [
            message("hello"),
            JournalEvent::Thought {
                text: "thinking".to_owned(),
            },
            JournalEvent::ToolCall {
                title: "read".to_owned(),
                status: "pending".to_owned(),
            },
            JournalEvent::Permission {
                title: "write".to_owned(),
                status: "refused".to_owned(),
            },
        ] {
            let entry = JournalEntry {
                seq: 1,
                time: timestamp(UNIX_EPOCH),
                event,
            };
            let encoded = serde_json::to_string(&entry).expect("the entry encodes");
            assert!(!encoded.contains('\n'));
            let decoded: JournalEntry = serde_json::from_str(&encoded).expect("the entry decodes");
            assert_eq!(decoded, entry);
        }
    }

    #[test]
    fn an_empty_journal_tails_nothing_and_counts_zero() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        assert_eq!(journal.tail(TASK, 200), Ok(empty_tail()));
        assert_eq!(journal.count(TASK), Ok(0));
    }

    #[test]
    fn an_empty_append_writes_nothing() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        assert_eq!(journal.append(TASK, &[]), Ok(Vec::new()));
        assert!(!directory.path().join("journal").join(TASK).exists());
    }

    #[test]
    fn an_append_stamps_a_contiguous_sequence_and_a_time() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let entries = journal
            .append(TASK, &[message("one"), message("two")])
            .expect("the append succeeds");
        assert_eq!(
            entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        for entry in &entries {
            assert!(entry.time.ends_with('Z'), "{}", entry.time);
            assert_eq!(entry.time.len(), "2026-11-08T06:19:08.123Z".len());
        }
        assert_eq!(journal.count(TASK), Ok(2));
    }

    #[test]
    fn a_write_that_would_pass_the_budget_rotates_first() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, rotate_every_entry());
        write_entries(&journal, 3);
        assert_eq!(generations(&journal), vec![0, 1, 2]);
        assert_eq!(journal.count(TASK), Ok(3));
    }

    #[test]
    fn the_tail_spans_rotated_segments_in_order() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, rotate_every_entry());
        write_entries(&journal, 5);
        let tail = journal.tail(TASK, 10).expect("the tail reads");
        assert_eq!(tail.total, 5);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| match &entry.event {
                    JournalEvent::Message { text } => text.as_str(),
                    _ => "unexpected",
                })
                .collect::<Vec<_>>(),
            vec!["m0", "m1", "m2", "m3", "m4"]
        );
    }

    #[test]
    fn the_tail_is_bounded_by_the_limit() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        write_entries(&journal, 10);
        let tail = journal.tail(TASK, 3).expect("the tail reads");
        assert_eq!(tail.total, 10);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
        assert_eq!(journal.count(TASK), Ok(10));
    }

    #[test]
    fn a_zero_limit_tails_nothing() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        write_entries(&journal, 3);
        assert_eq!(
            journal.tail(TASK, 0),
            Ok(JournalTail {
                entries: Vec::new(),
                total: 3,
            })
        );
    }

    #[test]
    fn the_sequence_continues_after_reopening_the_root() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let config = rotate_every_entry();
        {
            let journal = journal(&directory, config);
            write_entries(&journal, 2);
        }
        let journal = journal(&directory, config);
        let entries = journal
            .append(TASK, &[message("next")])
            .expect("the append succeeds");
        assert_eq!(entries[0].seq, 3);
        assert_eq!(journal.count(TASK), Ok(3));
        assert_eq!(
            journal
                .tail(TASK, 1)
                .expect("the tail reads")
                .entries
                .first()
                .map(|entry| entry.seq),
            Some(3)
        );
    }

    #[test]
    fn a_reopened_cursor_measures_the_active_segment() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let line = serde_json::to_vec(&JournalEntry {
            seq: 1,
            time: timestamp(UNIX_EPOCH),
            event: message("m0"),
        })
        .expect("the entry encodes")
        .len()
            + 1;
        let config = JournalConfig {
            max_bytes: u64::try_from(2 * line).expect("the budget fits"),
            ..JournalConfig::default()
        };
        {
            let journal = journal(&directory, config);
            write_entries(&journal, 1);
        }
        let segment = directory.path().join("journal").join(TASK).join("0.jsonl");
        let stored = fs::metadata(&segment).expect("the segment exists").len();
        assert_eq!(
            stored,
            u64::try_from(line).expect("the line fits"),
            "the fixture pins the size"
        );

        let journal = journal(&directory, config);
        assert_eq!(
            cached_cursor(&journal, TASK).bytes,
            stored,
            "a reopened cursor starts at the segment's length, not at zero"
        );

        // Two more records fill the budget exactly and then cross it. A cursor starting
        // at zero would keep both in the first segment.
        write_entries(&journal, 2);
        assert_eq!(generations(&journal), vec![0, 1]);
        assert_eq!(journal.count(TASK), Ok(3));
    }

    #[test]
    fn retention_keeps_the_newest_segments() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                keep_segments: 3,
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 6);
        assert_eq!(generations(&journal), vec![3, 4, 5]);
        assert_eq!(
            journal
                .tail(TASK, 10)
                .expect("the tail reads")
                .entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
        assert_eq!(
            journal.count(TASK),
            Ok(3),
            "the pruned entries leave the count"
        );
    }

    #[test]
    fn retention_drops_a_segment_older_than_the_max_age() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                max_age: Duration::from_hours(1),
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 2);
        let expired = directory.path().join("journal").join(TASK).join("0.jsonl");
        File::options()
            .write(true)
            .open(&expired)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");

        write_entries(&journal, 1);
        assert_eq!(generations(&journal), vec![1, 2]);
        assert!(!expired.exists());
        assert_eq!(journal.count(TASK), Ok(2));
    }

    #[test]
    fn retention_drops_an_old_segment_on_a_plain_append() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let line = serde_json::to_vec(&JournalEntry {
            seq: 1,
            time: timestamp(UNIX_EPOCH),
            event: message("m0"),
        })
        .expect("the entry encodes")
        .len()
            + 1;
        let journal = journal(
            &directory,
            JournalConfig {
                max_bytes: u64::try_from(3 * line).expect("the budget fits"),
                max_age: Duration::from_hours(1),
                keep_segments: 8,
            },
        );
        write_entries(&journal, 4);
        assert_eq!(generations(&journal), vec![0, 1]);

        let expired = directory.path().join("journal").join(TASK).join("0.jsonl");
        File::options()
            .write(true)
            .open(&expired)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");

        write_entries(&journal, 1);
        assert_eq!(generations(&journal), vec![1]);
        assert_eq!(
            journal.count(TASK),
            Ok(2),
            "the old segment and its entries are gone"
        );
    }

    #[test]
    fn a_zero_max_age_disables_age_pruning() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                max_age: Duration::ZERO,
                keep_segments: 8,
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 2);
        let ancient = directory.path().join("journal").join(TASK).join("0.jsonl");
        File::options()
            .write(true)
            .open(&ancient)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(24 * 365))
            .expect("the mtime is set");

        write_entries(&journal, 1);
        assert_eq!(generations(&journal), vec![0, 1, 2]);
    }

    #[test]
    fn an_invalid_key_is_refused() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        for key in ["", "Upper", "a/b", "..", "a b", "task_1"] {
            assert_eq!(
                journal.append(key, &[message("x")]),
                Err(JournalError::InvalidKey(key.to_owned()))
            );
            assert_eq!(
                journal.tail(key, 1),
                Err(JournalError::InvalidKey(key.to_owned()))
            );
            assert_eq!(
                journal.count(key),
                Err(JournalError::InvalidKey(key.to_owned()))
            );
        }
        assert!(!directory.path().join("journal").join("Upper").exists());
    }

    #[test]
    fn a_torn_line_is_skipped_and_the_sequence_continues() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        write_entries(&journal, 2);
        let segment = directory.path().join("journal").join(TASK).join("0.jsonl");
        let mut file = File::options()
            .append(true)
            .open(&segment)
            .expect("the segment opens");
        file.write_all(b"{\"seq\":3,\"time\":\"")
            .expect("the torn line writes");
        drop(file);

        assert_eq!(journal.count(TASK), Ok(2));
        assert_eq!(
            journal
                .tail(TASK, 10)
                .expect("the tail reads")
                .entries
                .len(),
            2
        );

        let entries = journal
            .append(TASK, &[message("three")])
            .expect("the append succeeds");
        assert_eq!(entries[0].seq, 3);
        assert_eq!(journal.count(TASK), Ok(3));
    }

    #[test]
    fn a_torn_line_survives_a_reopen() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let config = JournalConfig::default();
        {
            let journal = journal(&directory, config);
            write_entries(&journal, 2);
        }
        let segment = directory.path().join("journal").join(TASK).join("0.jsonl");
        let mut file = File::options()
            .append(true)
            .open(&segment)
            .expect("the segment opens");
        file.write_all(b"not json").expect("the torn line writes");
        drop(file);

        let journal = journal(&directory, config);
        let entries = journal
            .append(TASK, &[message("three")])
            .expect("the append succeeds");
        assert_eq!(entries[0].seq, 3);
    }

    #[test]
    fn one_append_call_can_rotate_between_its_events() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, rotate_every_entry());
        let entries = journal
            .append(TASK, &[message("a"), message("b"), message("c")])
            .expect("the append succeeds");
        assert_eq!(
            entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(generations(&journal), vec![0, 1, 2]);
        assert_eq!(journal.count(TASK), Ok(3));
        let tail = journal.tail(TASK, 10).expect("the tail reads");
        assert_eq!(tail.total, 3);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn a_huge_message_is_clipped() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let huge = "x".repeat(MAX_RECORD_BYTES + 100);
        let entries = journal
            .append(TASK, &[message(&huge)])
            .expect("the append succeeds");
        let JournalEvent::Message { text } = &entries[0].event else {
            panic!("the entry is a message");
        };
        assert_eq!(text.len(), MAX_RECORD_BYTES);
        let stored = journal.tail(TASK, 1).expect("the tail reads");
        assert_eq!(stored.entries, entries);
        let bytes = fs::read(directory.path().join("journal").join(TASK).join("0.jsonl"))
            .expect("the segment reads");
        assert!(
            bytes.len() < 2 * MAX_RECORD_BYTES,
            "the stored line is bounded"
        );
    }

    #[test]
    fn a_huge_tool_title_is_clipped() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let entries = journal
            .append(
                TASK,
                &[JournalEvent::ToolCall {
                    title: "y".repeat(MAX_RECORD_BYTES + 10),
                    status: "completed".to_owned(),
                }],
            )
            .expect("the append succeeds");
        let JournalEvent::ToolCall { title, status } = &entries[0].event else {
            panic!("the entry is a tool call");
        };
        assert_eq!(title.len(), MAX_RECORD_BYTES);
        assert_eq!(status, "completed");
    }

    #[test]
    fn a_clip_lands_on_a_character_boundary() {
        let text = format!("a{}", "é".repeat(MAX_RECORD_BYTES));
        let JournalEvent::Message { text } = JournalEvent::Message { text }.clipped() else {
            panic!("the event stays a message");
        };
        // The subtraction is saturating so the case still compiles, and fails, when a
        // mutation lowers the cap to zero.
        assert_eq!(
            text,
            format!("a{}", "é".repeat(MAX_RECORD_BYTES.saturating_sub(1) / 2)),
            "the kept prefix stops before the character straddling the cap"
        );
        assert_eq!(text.len(), MAX_RECORD_BYTES.saturating_sub(1));
    }

    #[test]
    fn a_small_text_is_returned_whole() {
        let event = message("short");
        assert_eq!(event.clipped(), event);
        let exact = message(&"z".repeat(MAX_RECORD_BYTES));
        assert_eq!(exact.clipped(), exact);
    }

    #[test]
    fn only_a_field_past_the_cap_needs_clipping() {
        let over = "z".repeat(MAX_RECORD_BYTES + 1);
        let exact = "z".repeat(MAX_RECORD_BYTES);
        assert!(!message("short").needs_clip());
        assert!(!message(&exact).needs_clip());
        assert!(message(&over).needs_clip());
        assert!(JournalEvent::Thought { text: over.clone() }.needs_clip());
        assert!(
            JournalEvent::ToolCall {
                title: over.clone(),
                status: "completed".to_owned(),
            }
            .needs_clip()
        );
        assert!(
            JournalEvent::ToolCall {
                title: "read".to_owned(),
                status: over.clone(),
            }
            .needs_clip()
        );
        assert!(
            JournalEvent::Permission {
                title: "write".to_owned(),
                status: over,
            }
            .needs_clip()
        );
        assert!(
            !JournalEvent::Permission {
                title: "write".to_owned(),
                status: exact,
            }
            .needs_clip()
        );
    }

    #[test]
    fn a_small_record_decodes_unchanged() {
        let entry = JournalEntry {
            seq: 3,
            time: timestamp(UNIX_EPOCH),
            event: JournalEvent::ToolCall {
                title: "read".to_owned(),
                status: "completed".to_owned(),
            },
        };
        assert_eq!(decode(&line(&entry)), Some(entry));
    }

    #[test]
    fn a_stored_record_from_an_older_build_is_clipped_on_the_way_out() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let segment = directory.path().join("journal").join(TASK);
        fs::create_dir_all(&segment).expect("the task dir creates");
        let raw = serde_json::to_vec(&JournalEntry {
            seq: 1,
            time: timestamp(UNIX_EPOCH),
            event: message(&"q".repeat(2 * MAX_RECORD_BYTES)),
        })
        .expect("the record encodes");
        fs::write(segment.join("0.jsonl"), [raw, b"\n".to_vec()].concat())
            .expect("the oversized segment writes");

        assert_eq!(
            journal.count(TASK),
            Ok(1),
            "a cold count scans the stored record"
        );
        let tail = journal.tail(TASK, 1).expect("the tail reads");
        let JournalEvent::Message { text } = &tail.entries[0].event else {
            panic!("the entry is a message");
        };
        assert_eq!(text.len(), MAX_RECORD_BYTES);
        assert_eq!(tail.total, 1);
    }

    #[test]
    fn a_task_path_that_is_a_file_is_an_io_error() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let root = directory.path().join("journal");
        fs::create_dir_all(&root).expect("the root creates");
        fs::write(root.join(TASK), b"not a directory").expect("the path writes");
        let journal =
            FileJournal::open(&root, JournalConfig::default()).expect("the journal opens");
        assert!(matches!(journal.tail(TASK, 1), Err(JournalError::Io(_))));
        assert!(matches!(journal.count(TASK), Err(JournalError::Io(_))));
    }

    #[test]
    fn a_vanished_segment_is_a_missing_error() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let missing = directory.path().join("missing.jsonl");
        assert_eq!(
            read_lines(&missing),
            Err(JournalError::Missing(missing.display().to_string()))
        );
    }

    #[test]
    fn opening_the_root_sweeps_an_expired_segment() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let config = JournalConfig {
            max_age: Duration::from_hours(1),
            ..rotate_every_entry()
        };
        {
            let journal = journal(&directory, config);
            write_entries(&journal, 2);
        }
        let expired = directory.path().join("journal").join(TASK).join("0.jsonl");
        File::options()
            .write(true)
            .open(&expired)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");

        let journal = journal(&directory, config);
        assert!(!expired.exists(), "startup must sweep the root");
        assert!(
            directory
                .path()
                .join("journal")
                .join(TASK)
                .join("1.jsonl")
                .exists()
        );
        assert_eq!(journal.count(TASK), Ok(1));
    }

    #[test]
    fn the_open_sweep_caches_each_task_count() {
        let directory = tempfile::tempdir().expect("a temp dir");
        {
            let journal = journal(&directory, JournalConfig::default());
            write_entries(&journal, 2);
        }
        let journal = journal(&directory, JournalConfig::default());
        fs::remove_dir_all(directory.path().join("journal").join(TASK))
            .expect("the task directory removes");

        assert_eq!(
            journal.count(TASK),
            Ok(2),
            "the sweep counted the task, so a later request needs no scan"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_tail_whose_segment_turns_unreadable_fails() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        write_entries(&journal, 1);
        let segment = directory.path().join("journal").join(TASK).join("0.jsonl");
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o000))
            .expect("the segment turns unreadable");
        let unreadable = fs::read(&segment).is_err();
        let result = journal.tail(TASK, 1);
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o600))
            .expect("the segment is readable again");

        if !unreadable {
            // Root reads through the mode, so the failure cannot be staged.
            return;
        }
        assert!(
            matches!(result, Err(JournalError::Io(_))),
            "a permission failure is returned without a retry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cold_cursor_skips_an_unreadable_older_segment() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temp dir");
        let config = rotate_every_entry();
        {
            let journal = journal(&directory, config);
            write_entries(&journal, 2);
        }
        let older = directory.path().join("journal").join(TASK).join("0.jsonl");
        fs::set_permissions(&older, fs::Permissions::from_mode(0o000))
            .expect("the segment turns unreadable");
        if fs::read(&older).is_ok() {
            // Root reads through the mode, so an unreadable segment cannot be staged.
            fs::set_permissions(&older, fs::Permissions::from_mode(0o600))
                .expect("the segment is readable again");
            return;
        }

        let mut journal = journal(&directory, config);
        let appended = journal.append(TASK, &[message("next")]);
        let count = journal.count(TASK);
        fs::set_permissions(&older, fs::Permissions::from_mode(0o600))
            .expect("the segment is readable again");

        let entries = appended.expect("the cold cursor warms past the unreadable segment");
        assert_eq!(
            entries[0].seq, 3,
            "the newest segment still gives the sequence"
        );
        assert_eq!(count, Ok(2), "the count covers the readable segments");

        // The skipped segment is readable again, but the lower bound still does not hold
        // its record: an append that prunes nothing keeps the bound instead of re-reading
        // every segment, which would count that record and turn the count exact.
        journal
            .append(TASK, &[message("again")])
            .expect("the append succeeds");
        assert_eq!(
            journal.count(TASK),
            Ok(3),
            "the lower bound plus the appended entry"
        );
        let cursor = cached_cursor(&journal, TASK);
        assert!(
            !cursor.count_exact,
            "a no-removal append must not rebuild the count"
        );

        // The segment the cold cursor skipped is pruned now. Its record never entered the
        // count, so subtracting the removal would leave the count below the entries a tail
        // returns; only a rebuild from the surviving segments is right.
        journal.config.keep_segments = 2;
        let appended = journal
            .append(TASK, &[message("last")])
            .expect("the append succeeds");
        assert_eq!(appended[0].seq, 5);
        assert!(!older.exists(), "the pruned segment is gone");
        let cursor = cached_cursor(&journal, TASK);
        assert_eq!(
            cursor.count, 2,
            "the count is rebuilt from the segments that remain"
        );
        assert!(
            cursor.count_exact,
            "the unreadable segment is gone, so the rebuild is exact"
        );
        let tail = journal.tail(TASK, 2).expect("the tail reads");
        assert_eq!(tail.total, 2);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
    }

    #[test]
    fn keep_segments_of_one_keeps_only_the_active() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                keep_segments: 1,
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 3);
        assert_eq!(generations(&journal), vec![2]);
        assert_eq!(journal.count(TASK), Ok(1));
        let tail = journal.tail(TASK, 10).expect("the tail reads");
        assert_eq!(tail.total, 1);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn the_active_segment_survives_the_max_age() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                max_age: Duration::from_hours(1),
                ..JournalConfig::default()
            },
        );
        write_entries(&journal, 2);
        let active = directory.path().join("journal").join(TASK).join("0.jsonl");
        File::options()
            .write(true)
            .open(&active)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");

        write_entries(&journal, 1);
        assert!(active.exists(), "the active segment is never dropped");
        assert_eq!(journal.count(TASK), Ok(3));
    }

    #[test]
    fn the_tail_can_land_exactly_on_a_segment_boundary() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let line = serde_json::to_vec(&JournalEntry {
            seq: 1,
            time: timestamp(UNIX_EPOCH),
            event: message("m0"),
        })
        .expect("the entry encodes")
        .len()
            + 1;
        let journal = journal(
            &directory,
            JournalConfig {
                max_bytes: u64::try_from(2 * line).expect("the budget fits"),
                ..JournalConfig::default()
            },
        );
        write_entries(&journal, 4);
        assert_eq!(generations(&journal), vec![0, 1]);
        let tail = journal.tail(TASK, 2).expect("the tail reads");
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn a_complete_segment_is_not_padded_with_a_separator() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let path = directory.path().join("journal").join(TASK).join("0.jsonl");

        let first = journal
            .append(TASK, &[message("one")])
            .expect("the append succeeds");
        assert_eq!(fs::read(&path).expect("the segment reads"), line(&first[0]));

        let second = journal
            .append(TASK, &[message("two")])
            .expect("the append succeeds");
        let mut expected = line(&first[0]);
        expected.extend_from_slice(&line(&second[0]));
        assert_eq!(
            fs::read(&path).expect("the segment reads"),
            expected,
            "a complete record already ends the file"
        );
    }

    #[test]
    fn a_multi_event_append_keeps_one_batch_per_segment() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(&directory, JournalConfig::default());
        let path = directory.path().join("journal").join(TASK).join("0.jsonl");

        let first = journal
            .append(TASK, &[message("one")])
            .expect("the append succeeds");
        let mut torn = line(&first[0]);
        torn.pop();
        fs::write(&path, &torn).expect("the torn segment writes");

        let entries = journal
            .append(TASK, &[message("two"), message("three")])
            .expect("the append succeeds");
        let mut expected = torn;
        expected.push(b'\n');
        expected.extend_from_slice(&line(&entries[0]));
        expected.extend_from_slice(&line(&entries[1]));
        assert_eq!(
            fs::read(&path).expect("the segment reads"),
            expected,
            "the separator belongs once, before the first record of the batch"
        );
        assert_eq!(journal.count(TASK), Ok(3));
    }

    #[test]
    fn a_segment_is_expired_only_past_the_max_age() {
        let now = UNIX_EPOCH + Duration::from_hours(10);
        let max_age = Duration::from_hours(1);
        assert!(
            !expired(now - max_age, now, max_age),
            "a segment at exactly the max age is kept"
        );
        assert!(expired(
            now - max_age - Duration::from_millis(1),
            now,
            max_age
        ));
        assert!(!expired(now + Duration::from_hours(1), now, max_age));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_segment_removal_is_not_counted() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temp dir");
        let mut journal = journal(
            &directory,
            JournalConfig {
                max_age: Duration::from_hours(1),
                keep_segments: 2,
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 2);
        let task_dir = directory.path().join("journal").join(TASK);
        let expired = task_dir.join("0.jsonl");
        File::options()
            .write(true)
            .open(&expired)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");
        journal.config.keep_segments = 1;
        let probe = task_dir.join("probe");
        fs::write(&probe, b"x").expect("the probe writes");
        fs::set_permissions(&task_dir, fs::Permissions::from_mode(0o555))
            .expect("the directory turns read-only");
        if fs::remove_file(&probe).is_ok() {
            // Root ignores the directory mode, so a failed removal cannot be staged.
            fs::set_permissions(&task_dir, fs::Permissions::from_mode(0o755))
                .expect("the directory is writable again");
            return;
        }

        let result = journal.prune(TASK, true);
        fs::set_permissions(&task_dir, fs::Permissions::from_mode(0o755))
            .expect("the directory is writable again");

        // Whether the failed segment stays in the list is internal; the file's presence
        // and the untouched count are what a caller can observe.
        assert_eq!(
            result,
            Ok(Removed {
                segments: 0,
                entries: 0
            }),
            "the failed removal is not counted"
        );
        assert!(expired.exists(), "the unremovable segment stays");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_expired_segment_does_not_fail_the_append() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temp dir");
        let journal = journal(
            &directory,
            JournalConfig {
                max_age: Duration::from_hours(1),
                keep_segments: 8,
                ..rotate_every_entry()
            },
        );
        write_entries(&journal, 2);
        let task_dir = directory.path().join("journal").join(TASK);
        let expired = task_dir.join("0.jsonl");
        File::options()
            .write(true)
            .open(&expired)
            .expect("the segment opens")
            .set_modified(SystemTime::now() - Duration::from_hours(2))
            .expect("the mtime is set");
        fs::set_permissions(&expired, fs::Permissions::from_mode(0o000))
            .expect("the segment turns unreadable");
        if fs::read(&expired).is_ok() {
            // Root reads through the mode, so an unreadable segment cannot be staged.
            fs::set_permissions(&expired, fs::Permissions::from_mode(0o600))
                .expect("the segment is readable again");
            return;
        }

        let entries = journal.append(TASK, &[message("next")]);
        let count = journal.count(TASK);
        fs::set_permissions(&expired, fs::Permissions::from_mode(0o600))
            .expect("the segment is readable again");

        assert!(
            entries.is_ok(),
            "the append survives the unreadable segment"
        );
        assert!(expired.exists(), "the segment is not removed unread");
        assert_eq!(count, Ok(3), "the count still includes the unread segment");
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .expect("the path exists")
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn a_new_journal_root_and_segment_are_private() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let root = directory.path().join("journal");
        let journal =
            FileJournal::open(&root, JournalConfig::default()).expect("the journal opens");
        write_entries(&journal, 1);
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join(TASK)), 0o700);
        assert_eq!(mode(&root.join(TASK).join("0.jsonl")), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_root_keeps_its_mode_but_new_task_dirs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temp dir");
        let root = directory.path().join("journal");
        fs::create_dir(&root).expect("the root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("the mode sets");
        let journal =
            FileJournal::open(&root, JournalConfig::default()).expect("the journal opens");
        write_entries(&journal, 1);
        assert_eq!(mode(&root), 0o755);
        assert_eq!(mode(&root.join(TASK)), 0o700);
        assert_eq!(mode(&root.join(TASK).join("0.jsonl")), 0o600);
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let config = JournalConfig::default();
        assert_eq!(config.max_bytes, 4 * 1024 * 1024);
        assert_eq!(config.keep_segments, 8);
        assert_eq!(config.max_age, Duration::from_hours(24 * 7));
        assert_eq!(MAX_RECORD_BYTES, 64 * 1024);
    }

    #[test]
    fn every_error_variant_describes_itself() {
        let invalid = JournalError::InvalidKey("../escape".to_owned());
        assert!(invalid.to_string().contains("../escape"), "{invalid}");
        let missing = JournalError::Missing("/srv/mokmok/7.jsonl".to_owned());
        assert!(
            missing.to_string().contains("/srv/mokmok/7.jsonl"),
            "{missing}"
        );
        let io = JournalError::Io("/srv/mokmok: the disk is full".to_owned());
        assert!(io.to_string().contains("/srv/mokmok"), "{io}");
    }

    #[test]
    fn a_memory_journal_stamps_and_tails_entries() {
        let journal = MemoryJournal::new();
        let entries = journal
            .append(TASK, &[message("one"), message("two")])
            .expect("the append succeeds");
        assert_eq!(
            entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(journal.count(TASK), Ok(2));
        let tail = journal.tail(TASK, 1).expect("the tail reads");
        assert_eq!(tail.total, 2);
        assert_eq!(
            tail.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            journal.tail(TASK, 0),
            Ok(JournalTail {
                entries: Vec::new(),
                total: 2,
            })
        );
        assert_eq!(journal.tail("another-task", 5), Ok(empty_tail()));
        assert_eq!(
            journal.append("Upper", &[message("x")]),
            Err(JournalError::InvalidKey("Upper".to_owned()))
        );
    }

    #[test]
    fn an_empty_memory_append_creates_no_task() {
        let journal = MemoryJournal::new();
        assert_eq!(journal.append(TASK, &[]), Ok(Vec::new()));
        assert!(
            journal
                .tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty(),
            "an empty batch must not register the task"
        );
    }

    #[test]
    fn the_clock_formats_rfc3339_utc_with_milliseconds() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_hours(24)),
            "1970-01-02T00:00:00.000Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_794_118_748_123)),
            "2026-11-08T06:19:08.123Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_hours(474_768)),
            "2024-02-29T00:00:00.000Z"
        );
        // The day before a March and the last day of a 400-year era pin the
        // century and leap-year corrections in the civil conversion.
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_hours(59 * 24)),
            "1970-03-01T00:00:00.000Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_hours(11_016 * 24)),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH - Duration::from_hours(24)),
            "1970-01-01T00:00:00.000Z"
        );
    }
}

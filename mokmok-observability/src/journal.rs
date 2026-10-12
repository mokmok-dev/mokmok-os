//! A JSON-lines journal that keeps every appended entry across a restart.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use mokmok_protocol::Moment;

use crate::audit::{AuditEntry, AuditLog};
use crate::error::Error;

/// An append-only file of [`AuditEntry`] lines, one entry per line.
///
/// Appends open the file again each time and sync it before returning, so an entry that a caller
/// has been told about is on disk even if the process dies immediately after, and a reader that
/// opens the same path later sees it. The file grows without bound: it is a record, not a cache.
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Opens the journal at `path`, creating an empty one when the file is not there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Journal`] when the file cannot be created or opened for appending.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(Error::Journal)?;
        Ok(Self { path })
    }

    /// The path this journal appends to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AuditLog for Journal {
    fn append(
        &mut self,
        entry: AuditEntry,
    ) -> Result<(), Error> {
        let line = serde_json::to_string(&entry).map_err(Error::Encode)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(Error::Journal)?;
        file.write_all(line.as_bytes()).map_err(Error::Journal)?;
        file.write_all(b"\n").map_err(Error::Journal)?;
        // The entry is durable only once the bytes are on disk. A caller that has been told the
        // append succeeded may report the decision as recorded, so the sync happens here rather
        // than when the file is dropped.
        file.sync_all().map_err(Error::Journal)?;
        Ok(())
    }

    fn read(
        &self,
        since: Moment,
    ) -> Result<Vec<AuditEntry>, Error> {
        let text = std::fs::read_to_string(&self.path).map_err(Error::Journal)?;
        let mut entries = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_str(line).map_err(Error::Decode)?;
            if entry.at() >= since {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use mokmok_protocol::Moment;

    use super::Journal;
    use crate::audit::{AuditEntry, AuditKind, AuditLog, Outcome};
    use crate::support::{TempDir, actor};

    #[test]
    fn entries_read_back_in_order_with_their_instant_and_actor_after_a_reopen() {
        let dir = TempDir::new("journal-reopen");
        let path = dir.path().join("audit.jsonl");
        let principal = actor();
        let first = AuditEntry::new(
            Moment::from_millis(1_700_000_000_000),
            principal,
            AuditKind::Authorization,
            Outcome::Allowed,
        )
        .with_detail("read /etc/hosts");
        let second = AuditEntry::new(
            Moment::from_millis(1_700_000_000_500),
            principal,
            AuditKind::Configuration,
            Outcome::Succeeded,
        );

        {
            let mut journal = Journal::open(&path).unwrap();
            journal.append(first.clone()).unwrap();
            journal.append(second.clone()).unwrap();
        }

        let reopened = Journal::open(&path).unwrap();
        let entries = reopened.read(Moment::EPOCH).unwrap();

        assert_eq!(entries, vec![first, second]);
        assert_eq!(entries[0].at().as_millis(), 1_700_000_000_000);
        assert_eq!(entries[0].actor(), principal);
        assert_eq!(entries[1].at().as_millis(), 1_700_000_000_500);
        assert_eq!(entries[1].actor(), principal);
    }

    #[test]
    fn read_since_skips_the_entries_appended_before_the_instant() {
        let dir = TempDir::new("journal-since");
        let path = dir.path().join("audit.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        let early = AuditEntry::new(
            Moment::from_millis(10),
            actor(),
            AuditKind::Execution,
            Outcome::Failed,
        );
        let late = AuditEntry::new(
            Moment::from_millis(20),
            actor(),
            AuditKind::Execution,
            Outcome::Succeeded,
        );
        journal.append(early).unwrap();
        journal.append(late.clone()).unwrap();

        assert_eq!(journal.read(Moment::from_millis(20)).unwrap(), vec![late]);
    }
}

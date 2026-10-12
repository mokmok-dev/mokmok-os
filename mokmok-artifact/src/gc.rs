//! The collector that reclaims the space of unreferenced blobs.

use mokmok_protocol::Moment;

use crate::Error;
use crate::blob::BlobStore;
use crate::content_hash::ContentHash;
use crate::filesystem::{self, Entry};
use crate::metadata::ReferenceStore;
use crate::retention::Retention;

/// What one sweep did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// The blobs removed, oldest first.
    pub collected: Vec<ContentHash>,
    /// The staging files removed; an interrupted `put` leaves one.
    pub temporaries: usize,
    /// The complete-blob bytes still stored.
    pub bytes: u64,
    /// Whether the store still holds more than [`Retention::max_bytes`].
    ///
    /// Only referenced blobs can be left, because the sweep removed every unreferenced one it
    /// could.
    pub over_budget: bool,
}

/// Reclaims unreferenced blobs under a [`Retention`].
pub struct Collector<'a> {
    store: &'a BlobStore,
    references: &'a dyn ReferenceStore,
    retention: Retention,
}

impl<'a> Collector<'a> {
    /// Collects under `retention`, checking `references` for what to keep.
    #[must_use]
    pub const fn new(
        store: &'a BlobStore,
        references: &'a dyn ReferenceStore,
        retention: Retention,
    ) -> Self {
        Self {
            store,
            references,
            retention,
        }
    }

    /// Removes the blobs no reference names, with `at` as the clock.
    ///
    /// One pass can lose a race with a reference being published. Two things stop it: a blob
    /// younger than `Retention::min_age` is never a candidate, and every candidate's reference is
    /// checked again by [`ReferenceStore::referenced`] immediately before it is unlinked. A
    /// reference that appeared since the mark is therefore seen, and its blob is kept.
    ///
    /// When the store holds more than `Retention::max_bytes`, candidates are evicted oldest first
    /// and the sweep stops as soon as the store fits; otherwise every candidate is removed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the store cannot be scanned or a candidate cannot be removed,
    /// and [`Error::References`] when the reference registry cannot answer.
    pub fn sweep(
        &self,
        at: Moment,
    ) -> Result<Report, Error> {
        let entries = filesystem::scan(self.store.root())?;

        // A staging file holds no budget: an interrupted write cannot push a complete blob out.
        let used: u64 = entries
            .iter()
            .filter(|entry| entry.hash.is_some())
            .map(|entry| entry.bytes)
            .sum();
        let under_pressure = used > self.retention.max_bytes;

        let mut candidates = self.mark(entries, at)?;
        candidates.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut report = Report {
            bytes: used,
            ..Report::default()
        };
        let mut remaining = used;

        for candidate in candidates {
            // The mark answered for a reference source that may have moved since.
            if let Some(hash) = candidate.hash
                && self.references.referenced(hash)?
            {
                continue;
            }

            if !filesystem::remove(&candidate.path)? {
                continue;
            }

            match candidate.hash {
                Some(hash) => {
                    remaining = remaining.saturating_sub(candidate.bytes);
                    report.collected.push(hash);
                },
                None => report.temporaries += 1,
            }

            if under_pressure && remaining <= self.retention.max_bytes {
                break;
            }
        }

        report.bytes = remaining;
        report.over_budget = remaining > self.retention.max_bytes;

        Ok(report)
    }

    fn mark(
        &self,
        entries: Vec<Entry>,
        at: Moment,
    ) -> Result<Vec<Entry>, Error> {
        let mut candidates = Vec::new();

        for entry in entries {
            if at.since(entry.modified) < self.retention.min_age {
                continue;
            }

            if let Some(hash) = entry.hash
                && self.references.referenced(hash)?
            {
                continue;
            }

            candidates.push(entry);
        }

        Ok(candidates)
    }
}

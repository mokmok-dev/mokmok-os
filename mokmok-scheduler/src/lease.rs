//! The fence: generations, the leases carved from them, and the refusal they produce.

use core::fmt;

use mokmok_protocol::{Moment, WorkerID};

use crate::job::JobId;

/// The fence on one job: the newest generation it has issued.
///
/// A lease carries the generation it was granted under and every operation its holder performs
/// presents it. A job that has issued a newer generation refuses the older one, so the holder of a
/// lapsed lease cannot overwrite what the newer generation owns.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Generation(u64);

impl Generation {
    /// The generation a job that has never been claimed sits at is zero; the first claim issues
    /// generation one.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The number this generation wraps.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// The right to run one job until it expires.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Lease {
    job: JobId,
    holder: WorkerID,
    generation: Generation,
    expires: Moment,
}

impl Lease {
    /// The job this lease runs.
    #[must_use]
    pub const fn job(&self) -> JobId {
        self.job
    }

    #[must_use]
    pub const fn holder(&self) -> WorkerID {
        self.holder
    }

    /// The generation that fences every lease issued before this one.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// The instant this lease lapses.
    #[must_use]
    pub const fn expires(&self) -> Moment {
        self.expires
    }

    /// Whether the lease has lapsed at `at`.
    ///
    /// A lease expires at the instant it names, so `at == expires` is already expired. Treating
    /// the named instant as still held would leave a window in which the holder and the next
    /// claimant both believe they hold the job.
    #[must_use]
    pub const fn is_expired(
        &self,
        at: Moment,
    ) -> bool {
        self.expires.as_millis() <= at.as_millis()
    }
}

/// A lease whose generation the job has moved past.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stale {
    job: JobId,
    presented: Generation,
    current: Option<Generation>,
}

impl Stale {
    #[must_use]
    pub const fn job(&self) -> JobId {
        self.job
    }

    /// The generation the refused lease presented.
    #[must_use]
    pub const fn presented(&self) -> Generation {
        self.presented
    }

    /// The job's newest generation, or `None` when this scheduler does not hold the job at all.
    #[must_use]
    pub const fn current(&self) -> Option<Generation> {
        self.current
    }
}

impl fmt::Display for Stale {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self.current {
            Some(current) => write!(
                formatter,
                "job {} presented generation {}, but the job is at {}",
                self.job.value(),
                self.presented.value(),
                current.value(),
            ),
            None => write!(
                formatter,
                "job {} presented generation {}, but no such job exists",
                self.job.value(),
                self.presented.value(),
            ),
        }
    }
}

impl core::error::Error for Stale {}

/// Builds a [`Stale`].
///
/// A free function rather than an associated one: [`Stale`] is re-exported, so a `pub` inherent
/// constructor would join the crate's public surface, while this module stays private.
#[must_use]
pub const fn stale(
    job: JobId,
    presented: Generation,
    current: Option<Generation>,
) -> Stale {
    Stale {
        job,
        presented,
        current,
    }
}

/// Builds the lease a claim grants.
#[must_use]
pub const fn grant(
    job: JobId,
    holder: WorkerID,
    generation: Generation,
    expires: Moment,
) -> Lease {
    Lease {
        job,
        holder,
        generation,
        expires,
    }
}

/// The same lease, extended to a later expiry.
#[must_use]
pub const fn extended(
    lease: Lease,
    expires: Moment,
) -> Lease {
    Lease { expires, ..lease }
}

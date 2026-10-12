//! Why a claim is refused and why a snapshot will not restore.

use alloc::string::String;
use core::fmt;

use mokmok_protocol::{Moment, WorkerID};

use crate::job::JobId;

/// Why a claim is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimError {
    /// No job in this scheduler has this id.
    Unknown(JobId),
    /// The job's next instant is later than the claim's, so it is scheduled but not yet eligible.
    NotDue(Moment),
    /// The job finished, so it admits no further lease.
    Finished,
    /// Another holder's lease is still live.
    Held(WorkerID),
}

impl fmt::Display for ClaimError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Unknown(job) => write!(formatter, "no job {}", job.value()),
            Self::NotDue(at) => write!(formatter, "the job is not due before {}", at.as_millis()),
            Self::Finished => formatter.write_str("the job has finished"),
            Self::Held(holder) => {
                write!(formatter, "a lease held by worker {}", holder.as_uuid())
            },
        }
    }
}

impl core::error::Error for ClaimError {}

/// The snapshot does not describe a state a scheduler can hold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Corrupt {
    message: String,
}

impl Corrupt {
    /// The inconsistency [`Scheduler::restore`](crate::Scheduler::restore) found.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Corrupt {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for Corrupt {}

/// Builds a [`Corrupt`].
///
/// A free function rather than an associated one: [`Corrupt`] is re-exported, so a `pub` inherent
/// constructor would join the crate's public surface, while this module stays private.
#[must_use]
pub fn corrupt(message: &str) -> Corrupt {
    Corrupt {
        message: String::from(message),
    }
}

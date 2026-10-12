//! Turns persisted timing and wake conditions into work claims and fenced leases.
//!
//! A [`Scheduler`] holds [`Job`]s and hands a worker a [`Lease`]. The [`Generation`] on that lease
//! is the fence: every operation a lease holder performs presents its lease, and a generation the
//! job has moved past is refused with [`Stale`] instead of writing. A worker that lost its lease
//! therefore cannot overwrite what a newer generation owns.
//!
//! Time enters through the [`Clock`] port and every operation takes the instant it acts at, so a
//! caller replaying recorded instants rebuilds the same state. [`Scheduler::snapshot`] and
//! [`Scheduler::restore`] are the durable boundary: the caller stores the snapshot in the same
//! transaction as the event it describes, so a restart resumes the jobs and their fences.
//!
//! # Examples
//!
//! ```
//! use core::time::Duration;
//! use mokmok_protocol::{Moment, WorkerID};
//! use mokmok_scheduler::{Clock, JobState, Outcome, Schedule, Scheduler};
//! use uuid::Uuid;
//!
//! struct Frozen(Moment);
//!
//! impl Clock for Frozen {
//!     fn now(&self) -> Moment {
//!         self.0
//!     }
//! }
//!
//! let holder = WorkerID::from_uuid(Uuid::from_bytes([0; 16]));
//! let at = Moment::from_millis(1_000);
//! let mut scheduler = Scheduler::new(Frozen(at));
//!
//! let job = scheduler.add(Schedule::Once(at), at);
//! let lease = scheduler.claim(job, holder, Duration::from_secs(30), at)?;
//! scheduler.settle(&lease, Outcome::Succeeded, at)?;
//!
//! assert_eq!(
//!     scheduler.job(job).map(|job| job.state()),
//!     Some(JobState::Succeeded),
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![no_std]

extern crate alloc;

mod clock;
mod error;
mod job;
mod lease;
mod scheduler;

pub use clock::Clock;
pub use error::{ClaimError, Corrupt};
pub use job::{Job, JobId, JobState, Outcome, Schedule};
pub use lease::{Generation, Lease, Stale};
pub use scheduler::{Scheduler, Snapshot};

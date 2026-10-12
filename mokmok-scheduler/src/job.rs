//! Jobs, their schedules, and the transitions a claim or a settlement drives.

use core::time::Duration;

use mokmok_protocol::{Moment, WorkerID};

use crate::lease::{Generation, Lease, extended, grant};

/// How many failed attempts a job is allowed before it dies.
const MAX_ATTEMPTS: u32 = 3;

/// The delay one failed attempt adds before the next, scaled by the attempt count.
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// Milliseconds in one second.
const MILLIS_PER_SECOND: u64 = 1_000;

/// The whole duration in milliseconds, clamped at the end of the range `u64` names.
///
/// `Duration` is seconds plus a sub-second remainder, so the two parts are scaled and added rather
/// than converted as one value: `u128::try_into` is not usable in a `const fn`, and this keeps the
/// callers that can be const const.
const fn whole_millis(by: Duration) -> u64 {
    by.as_secs()
        .saturating_mul(MILLIS_PER_SECOND)
        .saturating_add(by.subsec_millis() as u64)
}

/// Names one job for as long as its scheduler holds it.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct JobId(u64);

impl JobId {
    /// The number this id wraps.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// When a job becomes eligible.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    /// Eligible at this instant and, once it settles, done.
    Once(Moment),
    /// Eligible this long after the instant [`Scheduler::add`](crate::Scheduler::add) was called.
    After(Duration),
    /// Eligible at `anchor` and then every `every`, both measured from `anchor`.
    Interval { anchor: Moment, every: Duration },
}

/// What a lease holder reports about the attempt it ran.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The work finished.
    Succeeded,
    /// The attempt failed, so the job retries or dies.
    Failed,
}

/// Where a job is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for its next instant.
    Pending,
    /// Held by the worker this lease names.
    Leased(Lease),
    /// A one-shot job that finished; terminal.
    Succeeded,
    /// Waiting to retry, with this many attempts recorded.
    Failed { attempts: u32 },
    /// Out of attempts; terminal.
    Dead,
}

impl JobState {
    /// Whether the job admits no further lease.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Dead)
    }
}

/// One unit of scheduled work.
///
/// The attempt count lives here rather than only in [`JobState::Failed`], because a claim replaces
/// that state with a lease and the count has to survive into the settlement that follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Job {
    id: JobId,
    schedule: Schedule,
    state: JobState,
    generation: Generation,
    attempts: u32,
    next_at: Option<Moment>,
}

impl Job {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn schedule(&self) -> Schedule {
        self.schedule
    }

    #[must_use]
    pub const fn state(&self) -> JobState {
        self.state
    }

    /// The newest generation this job has issued, which fences every lease before it.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// The instant the job next becomes eligible, or `None` once it is terminal.
    #[must_use]
    pub const fn next_at(&self) -> Option<Moment> {
        self.next_at
    }

    /// Whether a claim at `at` is admitted by the schedule.
    ///
    /// The named instant itself is eligible, so `at == next_at` is due. A leased job is not due
    /// even when its lease has lapsed; [`Scheduler::expire`](crate::Scheduler::expire) is what
    /// returns it.
    #[must_use]
    pub const fn is_due(
        &self,
        at: Moment,
    ) -> bool {
        match self.state {
            JobState::Pending | JobState::Failed { .. } => match self.next_at {
                Some(next) => next.as_millis() <= at.as_millis(),
                None => false,
            },
            JobState::Leased(_) | JobState::Succeeded | JobState::Dead => false,
        }
    }
}

/// Mints the id `add` names a new job with.
#[must_use]
pub const fn job_id(value: u64) -> JobId {
    JobId(value)
}

/// Builds the job `add` stores.
#[must_use]
pub const fn new_job(
    id: JobId,
    schedule: Schedule,
    next_at: Moment,
) -> Job {
    Job {
        id,
        schedule,
        state: JobState::Pending,
        generation: Generation::new(0),
        attempts: 0,
        next_at: Some(next_at),
    }
}

/// The instant a fresh schedule first becomes eligible.
#[must_use]
pub const fn first_instant(
    schedule: Schedule,
    at: Moment,
) -> Moment {
    match schedule {
        Schedule::Once(instant) => instant,
        Schedule::After(delay) => at.saturating_add(delay),
        Schedule::Interval { anchor, .. } => anchor,
    }
}

/// Grants a lease, advancing the generation that fences every lease already issued.
pub const fn issue(
    job: &mut Job,
    holder: WorkerID,
    ttl: Duration,
    at: Moment,
) -> Lease {
    let generation = Generation::new(job.generation.value().saturating_add(1));
    let lease = grant(job.id, holder, generation, at.saturating_add(ttl));
    job.generation = generation;
    job.state = JobState::Leased(lease);
    lease
}

/// Extends the lease the job holds, keeping its generation and holder.
pub const fn extend(
    job: &mut Job,
    ttl: Duration,
    at: Moment,
) -> Option<Lease> {
    let JobState::Leased(current) = job.state else {
        return None;
    };
    let renewed = extended(current, at.saturating_add(ttl));
    job.state = JobState::Leased(renewed);
    Some(renewed)
}

/// Records a successful attempt: a one-shot job ends, an interval job re-arms on its grid.
pub const fn succeed(
    job: &mut Job,
    at: Moment,
) {
    match job.schedule {
        Schedule::Interval { anchor, every } => {
            job.state = JobState::Pending;
            job.next_at = Some(next_grid(anchor, every, at));
        },
        Schedule::Once(_) | Schedule::After(_) => {
            job.state = JobState::Succeeded;
            job.next_at = None;
        },
    }
}

/// Records a failed attempt: the next instant is measured from the settlement, and the attempt
/// that reaches [`MAX_ATTEMPTS`] is the last one.
pub const fn fail(
    job: &mut Job,
    at: Moment,
) {
    job.attempts = job.attempts.saturating_add(1);
    if job.attempts >= MAX_ATTEMPTS {
        job.state = JobState::Dead;
        job.next_at = None;
    } else {
        job.state = JobState::Failed {
            attempts: job.attempts,
        };
        job.next_at = Some(at.saturating_add(retry_delay(job.attempts)));
    }
}

/// Returns a job whose lease lapsed to the pool, at the expire instant and under a new generation.
pub const fn recover(
    job: &mut Job,
    at: Moment,
) {
    job.generation = Generation::new(job.generation.value().saturating_add(1));
    job.state = JobState::Pending;
    job.next_at = Some(at);
}

/// The delay before the attempt after `attempts` failures.
const fn retry_delay(attempts: u32) -> Duration {
    Duration::from_millis(whole_millis(RETRY_BACKOFF).saturating_mul(attempts as u64))
}

/// The first grid instant strictly after `at`, counted from `anchor`.
///
/// Counting from the anchor rather than from the previous wakeup is what keeps a late settlement
/// from drifting the grid: every interval job re-arms on the same instants no matter when it was
/// actually run.
const fn next_grid(
    anchor: Moment,
    every: Duration,
    at: Moment,
) -> Moment {
    let step = whole_millis(every);
    if step == 0 {
        return at;
    }
    let steps = at.as_millis().saturating_sub(anchor.as_millis()) / step + 1;
    anchor.saturating_add(Duration::from_millis(step.saturating_mul(steps)))
}

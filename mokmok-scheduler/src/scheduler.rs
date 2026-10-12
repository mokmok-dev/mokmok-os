//! The job table, the lease grants, and the durable snapshot.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::time::Duration;

use mokmok_protocol::{Moment, WorkerID};

use crate::clock::Clock;
use crate::error::{ClaimError, Corrupt, corrupt};
use crate::job::{
    Job, JobId, JobState, Outcome, Schedule, extend, fail, first_instant, issue, job_id, new_job,
    recover, succeed,
};
use crate::lease::{Lease, Stale, stale};

/// Holds jobs and hands out the fenced leases that claim them.
///
/// Time is a parameter, not a reading: every operation takes the instant it acts at, so a caller
/// replaying recorded instants rebuilds the same state. The held [`Clock`] is what a caller reads
/// when it has a fresh instant to begin a run with instead.
pub struct Scheduler<C: Clock> {
    clock: C,
    jobs: BTreeMap<JobId, Job>,
    next_id: u64,
}

/// The durable form of a scheduler's state.
///
/// The caller stores it in the same transaction as the event it describes, so a restart can resume
/// the jobs and the generations that fence them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Snapshot {
    next_id: u64,
    jobs: Vec<Job>,
}

impl<C: Clock> Scheduler<C> {
    /// A scheduler with no jobs, holding `clock`.
    #[must_use]
    pub const fn new(clock: C) -> Self {
        Self {
            clock,
            jobs: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// The clock's current reading.
    #[must_use]
    pub fn now(&self) -> Moment {
        self.clock.now()
    }

    /// Adds a job and returns the id that names it.
    ///
    /// `at` anchors a relative schedule: [`Schedule::After`] counts from it and an absolute
    /// [`Schedule::Once`] or [`Schedule::Interval`] ignores it.
    #[must_use]
    pub fn add(
        &mut self,
        schedule: Schedule,
        at: Moment,
    ) -> JobId {
        let id = job_id(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.jobs
            .insert(id, new_job(id, schedule, first_instant(schedule, at)));
        id
    }

    /// The jobs eligible at `at`, in id order.
    ///
    /// A leased job is not listed even when its lease has lapsed; [`Scheduler::expire`] returns it.
    #[must_use]
    pub fn due(
        &self,
        at: Moment,
    ) -> Vec<JobId> {
        self.jobs
            .values()
            .filter(|job| job.is_due(at))
            .map(Job::id)
            .collect()
    }

    /// Grants a lease on `job` to `holder` for `ttl`, or reports why it cannot.
    ///
    /// The same holder claiming a job it already holds gets that lease back unchanged, so a
    /// duplicate wakeup is not a second claim. A lapsed lease is claimable, and the new claim
    /// advances the generation that fences the old holder.
    ///
    /// # Errors
    ///
    /// Returns [`ClaimError::Unknown`] for an id this scheduler does not hold,
    /// [`ClaimError::Finished`] for a job that succeeded or died, [`ClaimError::NotDue`] before the
    /// job's next instant, and [`ClaimError::Held`] while another holder's lease is live.
    pub fn claim(
        &mut self,
        job: JobId,
        holder: WorkerID,
        ttl: Duration,
        at: Moment,
    ) -> Result<Lease, ClaimError> {
        let Some(entry) = self.jobs.get_mut(&job) else {
            return Err(ClaimError::Unknown(job));
        };
        match entry.state() {
            JobState::Succeeded | JobState::Dead => Err(ClaimError::Finished),
            JobState::Pending | JobState::Failed { .. } => match entry.next_at() {
                Some(next) if next.as_millis() <= at.as_millis() => {
                    Ok(issue(entry, holder, ttl, at))
                },
                Some(next) => Err(ClaimError::NotDue(next)),
                None => Err(ClaimError::Finished),
            },
            JobState::Leased(lease) => {
                if lease.is_expired(at) {
                    Ok(issue(entry, holder, ttl, at))
                } else if lease.holder() == holder {
                    Ok(lease)
                } else {
                    Err(ClaimError::Held(lease.holder()))
                }
            },
        }
    }

    /// Extends `lease` by `ttl` from `at`, keeping its generation.
    ///
    /// # Errors
    ///
    /// Returns [`Stale`] unless the job still holds this exact generation.
    pub fn renew(
        &mut self,
        lease: &Lease,
        ttl: Duration,
        at: Moment,
    ) -> Result<Lease, Stale> {
        let id = lease.job();
        let Some(entry) = self.jobs.get_mut(&id) else {
            return Err(stale(id, lease.generation(), None));
        };
        let JobState::Leased(held) = entry.state() else {
            return Err(stale(id, lease.generation(), Some(entry.generation())));
        };
        if held.generation() != lease.generation() {
            return Err(stale(id, lease.generation(), Some(entry.generation())));
        }
        extend(entry, ttl, at)
            .ok_or_else(|| stale(id, lease.generation(), Some(entry.generation())))
    }

    /// Records the outcome of the attempt `lease` covers.
    ///
    /// # Errors
    ///
    /// Returns [`Stale`] unless the job still holds this exact generation.
    pub fn settle(
        &mut self,
        lease: &Lease,
        outcome: Outcome,
        at: Moment,
    ) -> Result<(), Stale> {
        let id = lease.job();
        let Some(entry) = self.jobs.get_mut(&id) else {
            return Err(stale(id, lease.generation(), None));
        };
        let JobState::Leased(held) = entry.state() else {
            return Err(stale(id, lease.generation(), Some(entry.generation())));
        };
        if held.generation() != lease.generation() {
            return Err(stale(id, lease.generation(), Some(entry.generation())));
        }
        match outcome {
            Outcome::Succeeded => succeed(entry, at),
            Outcome::Failed => fail(entry, at),
        }
        Ok(())
    }

    /// Returns every job whose lease has lapsed at `at` to [`JobState::Pending`] under a new
    /// generation, in id order, so the caller can re-dispatch them.
    #[must_use]
    pub fn expire(
        &mut self,
        at: Moment,
    ) -> Vec<JobId> {
        let mut recovered = Vec::new();
        for entry in self.jobs.values_mut() {
            let lapsed = match entry.state() {
                JobState::Leased(lease) => lease.is_expired(at),
                _ => false,
            };
            if lapsed {
                recover(entry, at);
                recovered.push(entry.id());
            }
        }
        recovered
    }

    /// The durable form a caller stores in the same transaction as the event it describes.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            next_id: self.next_id,
            jobs: self.jobs.values().copied().collect(),
        }
    }

    /// Replaces this scheduler's jobs with a stored snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`Corrupt`] when the snapshot is not a state this scheduler could have produced;
    /// the scheduler is then left as it was.
    pub fn restore(
        &mut self,
        snapshot: Snapshot,
    ) -> Result<(), Corrupt> {
        let Snapshot { next_id, jobs } = snapshot;
        validate(&jobs, next_id)?;
        self.jobs = jobs.into_iter().map(|job| (job.id(), job)).collect();
        self.next_id = next_id;
        Ok(())
    }

    /// The job named by `id`, as a copy.
    #[must_use]
    pub fn job(
        &self,
        id: JobId,
    ) -> Option<Job> {
        self.jobs.get(&id).copied()
    }

    /// Every job, in id order.
    pub fn jobs(&self) -> impl Iterator<Item = (JobId, Job)> + '_ {
        self.jobs.iter().map(|(id, job)| (*id, *job))
    }
}

/// Whether a stored snapshot is a state the transitions above could have produced.
fn validate(
    jobs: &[Job],
    next_id: u64,
) -> Result<(), Corrupt> {
    if next_id == 0 {
        return Err(corrupt("the next job id is never zero"));
    }
    let mut ids = BTreeSet::new();
    for job in jobs {
        if job.id().value() >= next_id {
            return Err(corrupt("the next job id is not past every listed job"));
        }
        if !ids.insert(job.id()) {
            return Err(corrupt("a job id is listed twice"));
        }
        match job.state() {
            JobState::Pending | JobState::Failed { .. } => {
                if job.next_at().is_none() {
                    return Err(corrupt("a pending or failed job names no next instant"));
                }
            },
            JobState::Succeeded | JobState::Dead => {
                if job.next_at().is_some() {
                    return Err(corrupt("a terminal job names a next instant"));
                }
            },
            JobState::Leased(lease) => {
                if lease.job() != job.id() {
                    return Err(corrupt("a lease names a job other than the one holding it"));
                }
            },
        }
        if let JobState::Failed { attempts } = job.state()
            && attempts != job.attempts()
        {
            return Err(corrupt(
                "a failed job's attempt count disagrees with its state",
            ));
        }
    }
    Ok(())
}

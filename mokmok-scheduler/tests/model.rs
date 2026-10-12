//! A model over generated operation sequences.

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};

use mokmok_protocol::{Moment, WorkerID};
use mokmok_scheduler::{Clock, Generation, JobId, Lease, Outcome, Schedule, Scheduler};
use proptest::prelude::*;
use uuid::Uuid;

struct Frozen;

impl Clock for Frozen {
    fn now(&self) -> Moment {
        Moment::EPOCH
    }
}

const fn worker(last: u8) -> WorkerID {
    let mut bytes = [0_u8; 16];
    bytes[15] = last;
    WorkerID::from_uuid(Uuid::from_bytes(bytes))
}

/// One step a caller could take against a scheduler.
#[derive(Clone, Copy, Debug)]
enum Op {
    Add { after: u64, every: Option<u64> },
    Claim { job: u8, holder: u8, ttl: u64 },
    Renew { lease: u8, ttl: u64 },
    Settle { lease: u8, failed: bool },
    Expire,
    Advance { delta: u64 },
}

fn operation() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0_u64..2_000, prop::option::of(1_u64..200))
            .prop_map(|(after, every)| Op::Add { after, every }),
        (any::<u8>(), any::<u8>(), 0_u64..2_000).prop_map(|(job, holder, ttl)| Op::Claim {
            job,
            holder,
            ttl
        }),
        (any::<u8>(), 0_u64..2_000).prop_map(|(lease, ttl)| Op::Renew { lease, ttl }),
        (any::<u8>(), any::<bool>()).prop_map(|(lease, failed)| Op::Settle { lease, failed }),
        Just(Op::Expire),
        (0_u64..2_000).prop_map(|delta| Op::Advance { delta }),
    ]
}

fn pick<T: Copy>(
    items: &[T],
    slot: u8,
) -> Option<T> {
    if items.is_empty() {
        None
    } else {
        Some(items[usize::from(slot) % items.len()])
    }
}

proptest! {
    #[test]
    fn generations_never_decrease_and_a_settled_job_is_never_pending_again(
        ops in prop::collection::vec(operation(), 1..40),
    ) {
        let mut scheduler = Scheduler::new(Frozen);
        let holders = [worker(1), worker(2)];
        let mut at = Moment::from_millis(1_000);
        let mut jobs: Vec<JobId> = Vec::new();
        let mut leases: Vec<Lease> = Vec::new();
        let mut last_generation: BTreeMap<JobId, Generation> = BTreeMap::new();
        let mut terminal: BTreeSet<JobId> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Add { after, every } => {
                    let schedule = every.map_or_else(
                        || Schedule::After(Duration::from_millis(after)),
                        |every| Schedule::Interval {
                            anchor: at,
                            every: Duration::from_millis(every),
                        },
                    );
                    jobs.push(scheduler.add(schedule, at));
                },
                Op::Claim { job, holder, ttl } => {
                    if let Some(id) = pick(&jobs, job)
                        && let Ok(lease) = scheduler
                            .claim(id, holders[usize::from(holder) % 2], Duration::from_millis(ttl), at)
                    {
                        leases.push(lease);
                    }
                },
                Op::Renew { lease, ttl } => {
                    if let Some(held) = pick(&leases, lease)
                        && let Ok(renewed) = scheduler.renew(&held, Duration::from_millis(ttl), at)
                    {
                        leases.push(renewed);
                    }
                },
                Op::Settle { lease, failed } => {
                    if let Some(held) = pick(&leases, lease) {
                        let outcome = if failed { Outcome::Failed } else { Outcome::Succeeded };
                        let _ = scheduler.settle(&held, outcome, at);
                    }
                },
                Op::Expire => {
                    let _ = scheduler.expire(at);
                },
                Op::Advance { delta } => {
                    at = at.saturating_add(Duration::from_millis(delta));
                },
            }

            for (id, job) in scheduler.jobs() {
                if let Some(previous) = last_generation.get(&id) {
                    prop_assert!(
                        job.generation() >= *previous,
                        "job {} fell from generation {} to {}",
                        id.value(),
                        previous.value(),
                        job.generation().value(),
                    );
                }
                last_generation.insert(id, job.generation());

                if terminal.contains(&id) {
                    // A settled interval job re-arms to pending on a success, so the states that
                    // count as settled here are the terminal Succeeded and Dead.
                    prop_assert!(
                        job.state().is_terminal(),
                        "job {} left its terminal state",
                        id.value(),
                    );
                }
                if job.state().is_terminal() {
                    terminal.insert(id);
                }
            }
        }
    }
}

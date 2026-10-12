//! Lease, fence, expiry, interval, retry, and restart behavior, each pinned to a literal.

use core::time::Duration;

use mokmok_protocol::{Moment, WorkerID};
use mokmok_scheduler::{
    ClaimError, Clock, Generation, JobState, Outcome, Schedule, Scheduler, Snapshot,
};
use uuid::Uuid;

/// A clock frozen at one instant; the operations take their instant as an argument.
struct Frozen(Moment);

impl Clock for Frozen {
    fn now(&self) -> Moment {
        self.0
    }
}

const START: Moment = Moment::from_millis(1_000);
const TTL: Duration = Duration::from_secs(30);

const fn worker(last: u8) -> WorkerID {
    let mut bytes = [0_u8; 16];
    bytes[15] = last;
    WorkerID::from_uuid(Uuid::from_bytes(bytes))
}

const fn scheduler() -> Scheduler<Frozen> {
    Scheduler::new(Frozen(START))
}

#[test]
fn the_scheduler_reports_its_clock() {
    assert_eq!(scheduler().now(), START);
}

#[test]
fn expiry_at_the_boundary_instant_returns_the_job() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let lease = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");

    assert_eq!(lease.expires(), Moment::from_millis(31_000));
    assert_eq!(scheduler.expire(Moment::from_millis(30_999)).len(), 0);
    assert_eq!(scheduler.expire(lease.expires()), vec![job]);
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Pending),
    );
    assert_eq!(
        scheduler.job(job).map(|job| job.generation()),
        Some(Generation::new(2)),
    );
}

#[test]
fn a_claim_before_the_scheduled_instant_is_refused() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(Moment::from_millis(5_000)), START);

    assert_eq!(
        scheduler.claim(job, worker(1), TTL, START),
        Err(ClaimError::NotDue(Moment::from_millis(5_000))),
    );
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Pending),
    );
}

#[test]
fn an_expired_lease_is_claimable_by_another_holder() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let first = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");

    let second = scheduler
        .claim(job, worker(2), TTL, first.expires())
        .expect("the lease lapsed at the instant it named");

    assert_eq!(second.generation(), Generation::new(2));
    assert_eq!(second.holder(), worker(2));
    assert_eq!(second.expires(), Moment::from_millis(61_000));
}

#[test]
fn another_holder_cannot_claim_a_live_lease() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let lease = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");

    assert_eq!(
        scheduler.claim(job, worker(2), TTL, Moment::from_millis(1_001)),
        Err(ClaimError::Held(worker(1))),
    );
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Leased(lease)),
    );
}

#[test]
fn the_same_holder_reclaims_and_keeps_its_generation() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let first = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");

    let again = scheduler
        .claim(job, worker(1), TTL, Moment::from_millis(1_001))
        .expect("a duplicate wakeup is not a second claim");

    assert_eq!(again, first);
    assert_eq!(again.generation(), Generation::new(1));
    assert_eq!(again.expires(), Moment::from_millis(31_000));
}

#[test]
fn renew_with_a_stale_generation_is_refused() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let old = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");
    assert_eq!(scheduler.expire(old.expires()), vec![job]);
    let newer = scheduler
        .claim(job, worker(2), TTL, old.expires())
        .expect("an expired lease is claimable");

    let refused = scheduler
        .renew(&old, TTL, Moment::from_millis(31_001))
        .expect_err("the job moved past generation one");

    assert_eq!(refused.job(), job);
    assert_eq!(refused.presented(), Generation::new(1));
    assert_eq!(refused.current(), Some(Generation::new(3)));

    let extended = scheduler
        .renew(&newer, TTL, Moment::from_millis(31_001))
        .expect("the current generation still renews");
    assert_eq!(extended.generation(), Generation::new(3));
    assert_eq!(extended.expires(), Moment::from_millis(61_001));
}

#[test]
fn settle_with_a_stale_generation_is_refused() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let old = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");
    assert_eq!(scheduler.expire(old.expires()), vec![job]);
    let newer = scheduler
        .claim(job, worker(2), TTL, old.expires())
        .expect("an expired lease is claimable");

    let refused = scheduler
        .settle(&old, Outcome::Succeeded, Moment::from_millis(31_001))
        .expect_err("the job moved past generation one");

    assert_eq!(refused.presented(), Generation::new(1));
    assert_eq!(refused.current(), Some(Generation::new(3)));
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Leased(newer)),
    );
}

#[test]
fn a_relative_schedule_counts_from_the_add_instant() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::After(Duration::from_millis(500)), START);

    assert_eq!(scheduler.due(Moment::from_millis(1_499)).len(), 0);
    assert_eq!(scheduler.due(Moment::from_millis(1_500)), vec![job]);
}

#[test]
fn interval_eligibility_is_computed_from_the_anchor() {
    let anchor = Moment::from_millis(1_000);
    let every = Duration::from_millis(100);
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Interval { anchor, every }, START);

    assert_eq!(scheduler.due(Moment::from_millis(999)).len(), 0);
    assert_eq!(scheduler.due(anchor), vec![job]);

    let lease = scheduler
        .claim(job, worker(1), TTL, anchor)
        .expect("the job is due at the anchor");
    scheduler
        .settle(&lease, Outcome::Succeeded, anchor)
        .expect("the holder settles its own lease");

    assert_eq!(scheduler.due(Moment::from_millis(1_099)).len(), 0);
    assert_eq!(scheduler.due(Moment::from_millis(1_100)), vec![job]);

    let late = Moment::from_millis(1_537);
    let lease = scheduler
        .claim(job, worker(1), TTL, late)
        .expect("a late wakeup still finds the job due");
    scheduler
        .settle(&lease, Outcome::Succeeded, late)
        .expect("the holder settles its own lease");

    assert_eq!(scheduler.due(Moment::from_millis(1_599)).len(), 0);
    assert_eq!(scheduler.due(Moment::from_millis(1_600)), vec![job]);
    assert_eq!(
        scheduler.job(job).map(|job| job.next_at()),
        Some(Some(Moment::from_millis(1_600))),
    );
}

#[test]
fn a_failed_attempt_schedules_its_retry_from_the_settlement_instant() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);

    let lease = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Failed, START)
        .expect("the holder settles its own lease");
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Failed { attempts: 1 }),
    );
    assert_eq!(
        scheduler.job(job).map(|job| job.next_at()),
        Some(Some(Moment::from_millis(31_000))),
    );

    let retry = Moment::from_millis(31_000);
    let lease = scheduler
        .claim(job, worker(1), TTL, retry)
        .expect("the retry instant is due");
    scheduler
        .settle(&lease, Outcome::Failed, retry)
        .expect("the holder settles its own lease");
    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Failed { attempts: 2 }),
    );
    assert_eq!(
        scheduler.job(job).map(|job| job.next_at()),
        Some(Some(Moment::from_millis(91_000))),
    );
}

#[test]
fn attempts_reaching_the_bound_move_the_job_to_dead() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);

    let mut at = START;
    for _ in 0..3 {
        let lease = scheduler
            .claim(job, worker(1), TTL, at)
            .expect("the job is due at its instant or retry");
        scheduler
            .settle(&lease, Outcome::Failed, at)
            .expect("the holder settles its own lease");
        if let Some(next) = scheduler.job(job).and_then(|job| job.next_at()) {
            at = next;
        }
    }

    assert_eq!(
        scheduler.job(job).map(|job| job.state()),
        Some(JobState::Dead)
    );
    assert_eq!(scheduler.job(job).map(|job| job.attempts()), Some(3));
    assert_eq!(scheduler.job(job).map(|job| job.next_at()), Some(None));
    assert_eq!(
        scheduler.claim(job, worker(1), TTL, Moment::from_millis(400_000)),
        Err(ClaimError::Finished),
    );
}

#[test]
fn a_restart_restores_leases_and_generations() {
    let mut scheduler = scheduler();
    let pending = scheduler.add(Schedule::Once(Moment::from_millis(500_000)), START);
    let leased = scheduler.add(Schedule::Once(START), START);
    let succeeded = scheduler.add(Schedule::Once(START), START);
    let failed = scheduler.add(Schedule::Once(START), START);
    let dead = scheduler.add(Schedule::Once(START), START);

    let held = scheduler
        .claim(leased, worker(1), TTL, START)
        .expect("the job is due at its instant");

    let lease = scheduler
        .claim(succeeded, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Succeeded, START)
        .expect("the holder settles its own lease");

    let lease = scheduler
        .claim(failed, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Failed, START)
        .expect("the holder settles its own lease");

    let mut at = START;
    for _ in 0..3 {
        let lease = scheduler
            .claim(dead, worker(1), TTL, at)
            .expect("the job is due at its instant or retry");
        scheduler
            .settle(&lease, Outcome::Failed, at)
            .expect("the holder settles its own lease");
        if let Some(next) = scheduler.job(dead).and_then(|job| job.next_at()) {
            at = next;
        }
    }

    let snapshot = scheduler.snapshot();
    let json = serde_json::to_string(&snapshot).expect("a snapshot serializes");
    let stored: Snapshot = serde_json::from_str(&json).expect("a snapshot deserializes");
    let mut restarted = Scheduler::new(Frozen(Moment::EPOCH));
    restarted
        .restore(stored)
        .expect("a stored snapshot restores");

    assert_eq!(restarted.snapshot(), snapshot);
    assert_eq!(
        restarted.job(pending).map(|job| job.state()),
        Some(JobState::Pending),
    );
    assert_eq!(
        restarted.job(leased).map(|job| job.state()),
        Some(JobState::Leased(held)),
    );
    assert_eq!(
        restarted.job(leased).map(|job| job.generation()),
        Some(Generation::new(1)),
    );
    assert_eq!(held.generation(), Generation::new(1));
    assert_eq!(held.expires(), Moment::from_millis(31_000));
    assert_eq!(
        restarted.job(succeeded).map(|job| job.state()),
        Some(JobState::Succeeded),
    );
    assert_eq!(
        restarted.job(failed).map(|job| job.state()),
        Some(JobState::Failed { attempts: 1 }),
    );
    assert_eq!(
        restarted.job(dead).map(|job| job.state()),
        Some(JobState::Dead),
    );
    assert_eq!(
        restarted.job(dead).map(|job| job.generation()),
        Some(Generation::new(3)),
    );
}

#[test]
fn a_snapshot_with_no_next_job_id_is_refused() {
    let mut scheduler = scheduler();
    let stored: Snapshot =
        serde_json::from_str(r#"{"next_id":0,"jobs":[]}"#).expect("a snapshot deserializes");

    let refused = scheduler.restore(stored).expect_err("zero is never an id");

    assert_eq!(refused.message(), "the next job id is never zero");
    assert_eq!(scheduler.add(Schedule::Once(START), START).value(), 1,);
}

#[test]
fn each_add_mints_the_next_id() {
    let mut scheduler = scheduler();

    let first = scheduler.add(Schedule::Once(START), START);
    let second = scheduler.add(Schedule::Once(START), START);

    assert_eq!(first.value(), 1);
    assert_eq!(second.value(), 2);
}

#[test]
fn only_a_succeeded_or_dead_job_is_terminal() {
    let mut scheduler = scheduler();

    let pending = scheduler.add(Schedule::Once(Moment::from_millis(500_000)), START);
    let leased = scheduler.add(Schedule::Once(START), START);
    scheduler
        .claim(leased, worker(1), TTL, START)
        .expect("the job is due at its instant");
    let succeeded = scheduler.add(Schedule::Once(START), START);
    let lease = scheduler
        .claim(succeeded, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Succeeded, START)
        .expect("the holder settles its own lease");
    let failed = scheduler.add(Schedule::Once(START), START);
    let lease = scheduler
        .claim(failed, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Failed, START)
        .expect("the holder settles its own lease");
    let dead = scheduler.add(Schedule::Once(START), START);
    let mut at = START;
    for _ in 0..3 {
        let lease = scheduler
            .claim(dead, worker(1), TTL, at)
            .expect("the job is due at its instant or retry");
        scheduler
            .settle(&lease, Outcome::Failed, at)
            .expect("the holder settles its own lease");
        if let Some(next) = scheduler.job(dead).and_then(|job| job.next_at()) {
            at = next;
        }
    }

    let states = [pending, leased, succeeded, failed, dead].map(|id| {
        scheduler
            .job(id)
            .expect("the job was added to this scheduler")
            .state()
    });
    assert_eq!(
        states.map(JobState::is_terminal),
        [false, false, true, false, true],
    );
}

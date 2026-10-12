//! The text each failure renders, pinned to a literal.

use core::time::Duration;

use mokmok_protocol::{Moment, WorkerID};
use mokmok_scheduler::{Clock, Outcome, Schedule, Scheduler, Snapshot};
use uuid::Uuid;

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
fn a_claim_error_names_the_reason_it_refuses() {
    let mut scheduler = scheduler();

    let not_due = scheduler.add(Schedule::Once(Moment::from_millis(5_000)), START);
    assert_eq!(
        scheduler
            .claim(not_due, worker(1), TTL, START)
            .expect_err("the job is not due before its instant")
            .to_string(),
        "the job is not due before 5000",
    );

    let held = scheduler.add(Schedule::Once(START), START);
    scheduler
        .claim(held, worker(1), TTL, START)
        .expect("the job is due at its instant");
    assert_eq!(
        scheduler
            .claim(held, worker(2), TTL, START)
            .expect_err("another holder owns a live lease")
            .to_string(),
        "a lease held by worker 00000000-0000-0000-0000-000000000001",
    );

    let finished = scheduler.add(Schedule::Once(START), START);
    let lease = scheduler
        .claim(finished, worker(1), TTL, START)
        .expect("the job is due at its instant");
    scheduler
        .settle(&lease, Outcome::Succeeded, START)
        .expect("the holder settles its own lease");
    assert_eq!(
        scheduler
            .claim(finished, worker(1), TTL, START)
            .expect_err("the job has finished")
            .to_string(),
        "the job has finished",
    );
}

#[test]
fn a_claim_error_for_an_unknown_job_names_it() {
    let mut source = scheduler();
    let missing = source.add(Schedule::Once(START), START);
    let mut scheduler = scheduler();

    assert_eq!(
        scheduler
            .claim(missing, worker(1), TTL, START)
            .expect_err("this scheduler holds no such job")
            .to_string(),
        "no job 1",
    );
}

#[test]
fn a_corrupt_snapshot_renders_the_inconsistency_it_holds() {
    let mut scheduler = scheduler();
    let stored: Snapshot =
        serde_json::from_str(r#"{"next_id":0,"jobs":[]}"#).expect("a snapshot deserializes");

    let refused = scheduler.restore(stored).expect_err("zero is never an id");

    assert_eq!(refused.to_string(), "the next job id is never zero");
}

#[test]
fn a_stale_lease_renders_the_generations_it_compares() {
    let mut scheduler = scheduler();
    let job = scheduler.add(Schedule::Once(START), START);
    let old = scheduler
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");
    assert_eq!(scheduler.expire(old.expires()), vec![job]);
    scheduler
        .claim(job, worker(2), TTL, old.expires())
        .expect("an expired lease is claimable");

    let refused = scheduler
        .renew(&old, TTL, Moment::from_millis(31_001))
        .expect_err("the job moved past generation one");

    assert_eq!(
        refused.to_string(),
        "job 1 presented generation 1, but the job is at 3",
    );
}

#[test]
fn a_stale_lease_for_a_job_this_scheduler_lost_names_no_generation() {
    let mut source = scheduler();
    let job = source.add(Schedule::Once(START), START);
    let lease = source
        .claim(job, worker(1), TTL, START)
        .expect("the job is due at its instant");
    let mut other = scheduler();

    let refused = other
        .settle(&lease, Outcome::Succeeded, START)
        .expect_err("this scheduler holds no such job");

    assert_eq!(
        refused.to_string(),
        "job 1 presented generation 1, but no such job exists",
    );
}

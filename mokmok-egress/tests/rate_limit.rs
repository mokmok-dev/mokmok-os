//! The token bucket, driven from instants the test chooses.

use std::time::Duration;

use mokmok_egress::RateLimit;
use mokmok_protocol::{Moment, PrincipalID};
use uuid::Uuid;

const fn caller(tag: u128) -> PrincipalID {
    PrincipalID::from_uuid(Uuid::from_u128(tag))
}

#[test]
fn a_bucket_refills_only_at_the_instant_supplied() {
    let mut limit = RateLimit::new(2, Duration::from_secs(1));
    let caller = caller(1);
    let start = Moment::from_millis(1_000);

    assert!(limit.acquire(caller, start));
    assert!(limit.acquire(caller, start));
    assert!(!limit.acquire(caller, start));
    assert!(!limit.acquire(
        caller,
        start.checked_add(Duration::from_millis(999)).unwrap()
    ));
    assert!(limit.acquire(caller, start.checked_add(Duration::from_secs(1)).unwrap()));
    assert!(!limit.acquire(caller, start.checked_add(Duration::from_secs(1)).unwrap()));
    assert!(limit.acquire(caller, start.checked_add(Duration::from_secs(2)).unwrap()));
}

#[test]
fn a_bucket_is_spent_per_caller() {
    let mut limit = RateLimit::new(1, Duration::from_secs(1));
    let start = Moment::from_millis(1_000);

    assert!(limit.acquire(caller(1), start));
    assert!(!limit.acquire(caller(1), start));
    assert!(limit.acquire(caller(2), start));
}

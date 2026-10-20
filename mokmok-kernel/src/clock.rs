//! The kernel's one boundary to wall time.
//!
//! Everything below the kernel takes a [`Moment`] it was handed. A daemon outlives a command, so it
//! reads this port at each command and step boundary rather than carrying its startup instant.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mokmok_protocol::Moment;

/// Supplies the instant one durable record or authority decision is stamped with.
pub trait Clock: Send + Sync {
    /// Reads the clock once.
    fn now(&self) -> Moment;
}

/// Wall time, read once per call.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Moment {
        Moment::from_millis(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                }),
        )
    }
}

/// A clock a test drives by hand, so an instant is an input rather than a race.
#[derive(Debug)]
pub struct FixedClock {
    now: Mutex<Moment>,
}

impl FixedClock {
    #[must_use]
    pub const fn new(at: Moment) -> Self {
        Self {
            now: Mutex::new(at),
        }
    }

    /// Moves the reading forward by `by`.
    pub fn advance(
        &self,
        by: Duration,
    ) {
        let mut held = self.handle();
        *held = held.saturating_add(by);
    }

    /// Replaces the reading.
    pub fn set(
        &self,
        at: Moment,
    ) {
        let mut held = self.handle();
        *held = at;
    }

    fn handle(&self) -> MutexGuard<'_, Moment> {
        // A panic in a test that held the lock leaves a value this clock can still read.
        self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Moment {
        *self.handle()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mokmok_protocol::Moment;

    use super::{Clock, FixedClock, SystemClock};

    #[test]
    fn a_fixed_clock_advances_by_the_duration_it_is_given() {
        let clock = FixedClock::new(Moment::from_millis(1_700_000_000_000));

        clock.advance(Duration::from_millis(2_500));
        assert_eq!(clock.now(), Moment::from_millis(1_700_000_002_500));

        clock.advance(Duration::from_secs(3));
        assert_eq!(clock.now(), Moment::from_millis(1_700_000_005_500));
    }

    #[test]
    fn a_fixed_clock_takes_a_new_reading() {
        let clock = FixedClock::new(Moment::EPOCH);

        clock.set(Moment::from_millis(42));
        assert_eq!(clock.now(), Moment::from_millis(42));
    }

    #[test]
    fn a_fixed_clock_clamps_an_advance_at_the_end_of_the_range() {
        let clock = FixedClock::new(Moment::from_millis(u64::MAX - 1));

        clock.advance(Duration::from_millis(10));
        assert_eq!(clock.now(), Moment::from_millis(u64::MAX));
    }

    #[test]
    fn the_system_clock_reads_an_instant_after_the_epoch() {
        assert!(
            SystemClock.now().as_millis() > 0,
            "wall time is after the epoch on every machine this runs on",
        );
    }
}

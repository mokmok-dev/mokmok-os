//! A wall-clock instant a pure crate can hold without the standard library.

use core::time::Duration;

/// Milliseconds in one second, the unit the two halves of a [`Duration`] are added in.
const MILLIS_PER_SECOND: u64 = 1_000;

/// The whole duration in milliseconds, clamped at the end of the range `u64` names.
///
/// `Duration` is seconds plus a sub-second remainder, so the two parts are scaled and added rather
/// than converted as one value: `u128::try_into` is not usable in a `const fn`, and this keeps every
/// function here const so a caller can pin one in a `const` initializer.
const fn whole_millis(by: Duration) -> u64 {
    by.as_secs()
        .saturating_mul(MILLIS_PER_SECOND)
        .saturating_add(by.subsec_millis() as u64)
}

/// Milliseconds since the Unix epoch.
///
/// A `Moment` is a value, not a reading. Nothing in this workspace reads a clock except the port
/// that supplies one, so a rule that compares instants stays reproducible in a test and a replay
/// of the same instant sequence reaches the same state.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Moment(u64);

impl Moment {
    /// The start of the epoch, the fixed point a test counts from.
    pub const EPOCH: Self = Self(0);

    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// The instant `by` after this one, or `None` when the sum leaves the range `u64` names.
    ///
    /// The caller chooses between this and [`Moment::saturating_add`]: a deadline that must be
    /// provably nameable fails here, and one that only needs an order clamps there.
    #[must_use]
    pub const fn checked_add(
        self,
        by: Duration,
    ) -> Option<Self> {
        match self.0.checked_add(whole_millis(by)) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    /// The instant `by` before this one, or `None` when that would precede the epoch.
    #[must_use]
    pub const fn checked_sub(
        self,
        by: Duration,
    ) -> Option<Self> {
        match self.0.checked_sub(whole_millis(by)) {
            Some(difference) => Some(Self(difference)),
            None => None,
        }
    }

    /// Clamps at the end of the range rather than wrapping, so an instant that cannot be named
    /// lands past every instant that can.
    #[must_use]
    pub const fn saturating_add(
        self,
        by: Duration,
    ) -> Self {
        Self(self.0.saturating_add(whole_millis(by)))
    }

    /// The time from `earlier` to this instant, and zero when `earlier` is later.
    ///
    /// Elapsed time has no negative value, and every caller here wants the age of something it
    /// just observed, so the clamp keeps the return total.
    #[must_use]
    pub const fn since(
        self,
        earlier: Self,
    ) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }
}

#[cfg(test)]
mod tests {
    use super::Moment;
    use core::time::Duration;
    use proptest::prelude::*;

    #[test]
    fn checked_add_names_the_sum() {
        let base = Moment::from_millis(1_700_000_000_000);

        assert_eq!(
            base.checked_add(Duration::from_millis(500)),
            Some(Moment::from_millis(1_700_000_000_500)),
        );
        assert_eq!(
            Moment::from_millis(u64::MAX).checked_add(Duration::from_millis(1)),
            None,
        );
    }

    #[test]
    fn checked_sub_refuses_to_precede_the_epoch() {
        assert_eq!(
            Moment::from_millis(1_000).checked_sub(Duration::from_millis(250)),
            Some(Moment::from_millis(750)),
        );
        assert_eq!(
            Moment::from_millis(100).checked_sub(Duration::from_millis(101)),
            None,
        );
    }

    #[test]
    fn a_subtraction_from_an_earlier_instant_is_zero() {
        assert_eq!(
            Moment::from_millis(10).since(Moment::from_millis(50)),
            Duration::ZERO,
        );
        assert_eq!(
            Moment::from_millis(50).since(Moment::from_millis(10)),
            Duration::from_millis(40),
        );
    }

    #[test]
    fn sub_second_precision_survives_the_scaling() {
        let base = Moment::EPOCH;

        assert_eq!(
            base.checked_add(Duration::from_millis(1_234)),
            Some(Moment::from_millis(1_234)),
        );
        assert_eq!(
            base.checked_add(Duration::new(3, 4_000_000)),
            Some(Moment::from_millis(3_004)),
        );
    }

    proptest! {
        #[test]
        fn checked_add_and_sub_are_inverse_where_both_are_named(
            millis in 0u64..=u64::MAX,
            by in 0u64..=u64::MAX,
        ) {
            let base = Moment::from_millis(millis);
            let delta = Duration::from_millis(by);

            if let Some(later) = base.checked_add(delta) {
                prop_assert_eq!(later.checked_sub(delta), Some(base));
            }
        }

        #[test]
        fn saturating_add_clamps_where_checked_add_gives_up(
            millis in 0u64..=u64::MAX,
            by in 0u64..=u64::MAX,
        ) {
            let base = Moment::from_millis(millis);
            let delta = Duration::from_millis(by);

            if let Some(exact) = base.checked_add(delta) {
                prop_assert_eq!(base.saturating_add(delta), exact);
            } else {
                prop_assert_eq!(base.saturating_add(delta), Moment::from_millis(u64::MAX));
            }
        }
    }
}

//! The port that supplies the current instant.

use mokmok_protocol::Moment;

/// The source of "now" for a [`Scheduler`](crate::Scheduler).
///
/// Every scheduler operation takes the instant it acts at as an argument, so replaying recorded
/// instants reaches the state the original run did without consulting this port. The adapter that
/// composes the scheduler reads it when it has a fresh instant to begin a run with instead.
pub trait Clock {
    /// The current instant.
    fn now(&self) -> Moment;
}

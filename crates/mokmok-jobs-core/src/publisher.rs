use mokmok_events::CloudEvent;

use crate::job::JobEvent;

/// The fan-out called after a successful append.
///
/// Delivery is at-least-once from the log, and an implementation must not panic.
/// The committed job's actor blocks in this call, so an implementation must not
/// send a command back to that same job.
pub trait EventPublisher: Send + Sync {
    fn publish(
        &self,
        events: &[CloudEvent<JobEvent>],
    );
}

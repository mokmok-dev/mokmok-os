//! A token bucket per caller, counted against a caller-supplied instant.

use std::collections::HashMap;
use std::time::Duration;

use mokmok_protocol::{Moment, PrincipalID};

struct Bucket {
    tokens: u32,
    refilled_at: Moment,
}

/// How many calls a caller may make before it has to wait for a refill.
///
/// The clock is a parameter, not a reading, so a caller can drive the bucket from any instant it
/// chooses and a test never sleeps.
pub struct RateLimit {
    capacity: u32,
    refill: Duration,
    buckets: HashMap<PrincipalID, Bucket>,
}

impl RateLimit {
    /// A bucket of `capacity` tokens that gains one token every `refill`.
    #[must_use]
    pub fn new(
        capacity: u32,
        refill: Duration,
    ) -> Self {
        Self {
            capacity,
            refill,
            buckets: HashMap::new(),
        }
    }

    /// Whether the call by `caller` at `now` is admitted, spending one token when it is.
    #[must_use]
    pub fn acquire(
        &mut self,
        caller: PrincipalID,
        now: Moment,
    ) -> bool {
        let capacity = self.capacity;
        let bucket = self.buckets.entry(caller).or_insert(Bucket {
            tokens: capacity,
            refilled_at: now,
        });

        // A call may name an instant before the one that filled the bucket, and `Duration` is not
        // negative, so elapsed time never subtracts.
        let elapsed = now.since(bucket.refilled_at).as_millis();
        let period = self.refill.as_millis().max(1);
        let steps = u32::try_from(elapsed / period)
            .unwrap_or(u32::MAX)
            .min(capacity);

        if steps > 0 {
            bucket.tokens = bucket.tokens.saturating_add(steps).min(capacity);
            // Advance by whole periods only, so the part of a period not yet elapsed still counts
            // toward the next token instead of being dropped.
            bucket.refilled_at = bucket.refilled_at.saturating_add(self.refill * steps);
        }

        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;

        true
    }
}

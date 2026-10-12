//! The retention contract the collector sweeps under.

use core::time::Duration;

/// How long the collector waits, and how many bytes the store may keep.
///
/// `min_age` is the grace period a reference is published inside: a blob younger than `now -
/// min_age` is not a candidate at all, so a reference that is still being written cannot lose a
/// race with the sweep. `max_bytes` is a ceiling, not a target: the collector never evicts a
/// referenced blob to reach it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Retention {
    /// The youngest a blob may be and still be collected.
    pub min_age: Duration,
    /// The byte ceiling over complete blobs.
    pub max_bytes: u64,
}

use std::num::{NonZeroU64, NonZeroUsize};

/// The resource limits one execution runs under.
///
/// These are the sandbox's own per-execution rlimits and its capture cap. The agent's execution
/// budget, which spans executions and counts tokens or turns, is a different responsibility and
/// lives with the agent runtime, not here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Limits {
    /// Bytes of address space. `None` leaves the inherited limit alone.
    pub address_space: Option<NonZeroU64>,
    /// Whole seconds of CPU time. `None` leaves the inherited limit alone.
    pub cpu: Option<NonZeroU64>,
    /// Bytes a single file may reach. `None` leaves the inherited limit alone.
    pub file_size: Option<NonZeroU64>,
    /// Open file descriptors. `None` leaves the inherited limit alone.
    pub open_files: Option<NonZeroU64>,
    /// Bytes captured per stream before the rest is discarded.
    pub output: NonZeroUsize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            address_space: None,
            cpu: None,
            file_size: None,
            open_files: None,
            output: mib(),
        }
    }
}

const fn mib() -> NonZeroUsize {
    match NonZeroUsize::new(1024 * 1024) {
        Some(cap) => cap,
        None => NonZeroUsize::MIN,
    }
}

//! A stream position.

use serde::{Deserialize, Serialize};

/// A one-based position in one event stream.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Version(u64);

impl Version {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_counts_forward_from_zero() {
        assert_eq!(Version::ZERO.get(), 0);
        assert_eq!(Version::new(7).next().get(), 8);
        assert!(Version::ZERO < Version::new(1));
    }
}

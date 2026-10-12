use core::num::{NonZeroU64, ParseIntError};
use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

/// Names one stream of events.
///
/// Persist the [`Display`](fmt::Display) form and rebuild it with [`FromStr`] to resume.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct StreamId(Uuid);

impl StreamId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for StreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StreamId {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for StreamId {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self).map_err(|_| InvalidId)
    }
}

/// Names one consumer across reconnects, so its checkpoint and inbox survive a restart.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct ConsumerId(Uuid);

impl ConsumerId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ConsumerId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConsumerId {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for ConsumerId {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self).map_err(|_| InvalidId)
    }
}

/// Names one command, which is also its idempotency key.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct CommandId(Uuid);

impl CommandId {
    /// Mints a fresh identity. A caller that retries with a fresh id commits a second time instead
    /// of replaying.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for CommandId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CommandId {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for CommandId {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self).map_err(|_| InvalidId)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, thiserror::Error)]
#[error("an id is a UUID")]
pub struct InvalidId;

/// How many events a stream holds after a commit. Zero-based.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Version(u64);

impl Version {
    /// The version of a stream that has never been appended to.
    pub const INITIAL: Self = Self(0);

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
        Self(self.0.saturating_add(1))
    }
}

/// The store-assigned position of one commit in the global log. One-based and dense, because it
/// is the `commit_log` table's rowid.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct CommitPosition(NonZeroU64);

impl CommitPosition {
    /// The first position a commit can take, which is where a whole-log read starts.
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    pub(crate) const fn from_nonzero(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match NonZeroU64::new(self.0.get().saturating_add(1)) {
            Some(value) => Self(value),
            None => self,
        }
    }
}

impl FromStr for CommitPosition {
    type Err = ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse::<NonZeroU64>().map(Self)
    }
}

/// What a caller believes the stream version is before an append lands.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Expected {
    /// Any version, including one another writer produced a moment ago.
    Any,
    /// The stream must not exist yet.
    New,
    /// The stream must be exactly this version.
    At(Version),
}

impl Expected {
    #[must_use]
    pub const fn accepts(
        self,
        found: Version,
    ) -> bool {
        match self {
            Self::Any => true,
            Self::New => found.get() == Version::INITIAL.get(),
            Self::At(version) => found.get() == version.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandId, CommitPosition, ConsumerId, Expected, InvalidId, StreamId, Version};

    #[test]
    fn an_id_round_trips_through_display_and_serde() {
        let stream = StreamId::new();

        assert_eq!(stream.to_string().parse::<StreamId>(), Ok(stream));
        let encoded = serde_json::to_string(&stream).unwrap();
        assert_eq!(encoded, format!("\"{stream}\""));
        assert_eq!(serde_json::from_str::<StreamId>(&encoded).unwrap(), stream);

        let consumer = ConsumerId::new();
        assert_eq!(consumer.to_string().parse::<ConsumerId>(), Ok(consumer));

        let command = CommandId::new();
        assert_eq!(command.to_string().parse::<CommandId>(), Ok(command));
    }

    #[test]
    fn a_bad_id_is_an_invalid_id() {
        assert_eq!("not-a-uuid".parse::<StreamId>(), Err(InvalidId));
    }

    #[test]
    fn an_id_defaults_to_a_fresh_identity() {
        assert_ne!(ConsumerId::default(), ConsumerId::default());
        assert_ne!(StreamId::default(), StreamId::default());
        assert_ne!(CommandId::default(), CommandId::default());
    }

    #[test]
    fn next_counts_one_more_event() {
        assert_eq!(Version::INITIAL.next(), Version::new(1));
        assert_eq!(Version::new(4).next(), Version::new(5));
    }

    #[test]
    fn accepts_compares_the_expectation_against_the_found_version() {
        assert!(Expected::Any.accepts(Version::INITIAL));
        assert!(Expected::Any.accepts(Version::new(3)));
        assert!(Expected::New.accepts(Version::INITIAL));
        assert!(!Expected::New.accepts(Version::new(3)));
        assert!(Expected::At(Version::new(3)).accepts(Version::new(3)));
        assert!(!Expected::At(Version::new(3)).accepts(Version::new(4)));
    }

    #[test]
    fn a_commit_position_parses_a_nonzero_integer_only() {
        assert_eq!(CommitPosition::FIRST.get(), 1);
        assert!("0".parse::<CommitPosition>().is_err());
        assert_eq!("2".parse::<CommitPosition>().unwrap().get(), 2);
        assert_eq!(
            CommitPosition::from_nonzero(core::num::NonZeroU64::MIN)
                .next()
                .get(),
            2
        );
    }
}

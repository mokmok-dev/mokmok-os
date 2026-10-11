use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use mokmok_protocol::EventID;

use crate::append::Committed;
use crate::model::{CommitPosition, StreamId, Version};
use crate::payload::Payload;

/// The name a stored event carries, which its consumer dispatches on.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Kind(String);

impl Kind {
    /// # Errors
    ///
    /// Returns an [`InvalidKind`] when `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidKind> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidKind);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Kind {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Kind {
    type Err = InvalidKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, thiserror::Error)]
#[error("an event kind is a non-empty name")]
pub struct InvalidKind;

/// The caller-supplied half of an event: what it is and what it carries.
#[derive(Debug)]
pub struct NewEvent {
    pub(crate) kind: Kind,
    pub(crate) payload: Payload,
}

impl NewEvent {
    #[must_use]
    pub const fn new(
        kind: Kind,
        payload: Payload,
    ) -> Self {
        Self { kind, payload }
    }
}

/// One stored event, complete with the identity and position the store gave it.
#[derive(Debug)]
pub struct Event {
    id: EventID,
    stream: StreamId,
    version: Version,
    position: CommitPosition,
    kind: Kind,
    payload: Payload,
    at: SystemTime,
}

impl Event {
    pub(crate) const fn new(
        id: EventID,
        stream: StreamId,
        version: Version,
        position: CommitPosition,
        kind: Kind,
        payload: Payload,
        at: SystemTime,
    ) -> Self {
        Self {
            id,
            stream,
            version,
            position,
            kind,
            payload,
            at,
        }
    }

    #[must_use]
    pub const fn id(&self) -> EventID {
        self.id
    }

    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    #[must_use]
    pub const fn position(&self) -> CommitPosition {
        self.position
    }

    #[must_use]
    pub const fn kind(&self) -> &Kind {
        &self.kind
    }

    #[must_use]
    pub const fn payload(&self) -> &Payload {
        &self.payload
    }

    #[must_use]
    pub const fn at(&self) -> SystemTime {
        self.at
    }
}

/// A stream's snapshot and the commits a read returned.
#[derive(Debug)]
pub struct Slice {
    version: Version,
    state: Option<Payload>,
    commits: Vec<Committed>,
}

impl Slice {
    pub(crate) const fn new(
        version: Version,
        state: Option<Payload>,
        commits: Vec<Committed>,
    ) -> Self {
        Self {
            version,
            state,
            commits,
        }
    }

    /// The stream's version after everything committed to it.
    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    #[must_use]
    pub const fn state(&self) -> Option<&Payload> {
        self.state.as_ref()
    }

    /// The commits this read returned, in position order.
    #[must_use]
    pub fn commits(&self) -> &[Committed] {
        &self.commits
    }

    /// Every event of every commit, in version order, for a caller replaying the stream.
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.commits.iter().flat_map(Committed::events)
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidKind, Kind};

    #[test]
    fn an_empty_kind_is_refused() {
        assert_eq!(Kind::new(""), Err(InvalidKind));
    }

    #[test]
    fn a_kind_round_trips_through_display_and_from_str() {
        let kind = Kind::new("turn.finished").unwrap();

        assert_eq!(kind.to_string(), "turn.finished");
        assert_eq!(kind.as_str(), "turn.finished");
        assert_eq!("turn.finished".parse::<Kind>(), Ok(kind));
        assert!("".parse::<Kind>().is_err());
    }
}

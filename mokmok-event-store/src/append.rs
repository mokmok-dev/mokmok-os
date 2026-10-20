use core::num::NonZeroU64;
use std::time::SystemTime;

use crate::event::{Event, NewEvent};
use crate::model::{CommandId, CommitPosition, Expected, StreamId, Version};
use crate::payload::Payload;

/// One command's writes, assembled before the store sees them.
///
/// [`Append::new`] takes the first event, so an append with no events is not representable.
#[derive(Debug)]
pub struct Append {
    pub(crate) command: CommandId,
    pub(crate) stream: StreamId,
    pub(crate) expected: Expected,
    pub(crate) first: NewEvent,
    pub(crate) rest: Vec<NewEvent>,
    pub(crate) state: Option<Payload>,
    pub(crate) outbox: Vec<OutboxMessage>,
}

impl Append {
    #[must_use]
    pub const fn new(
        command: CommandId,
        stream: StreamId,
        expected: Expected,
        first: NewEvent,
    ) -> Self {
        Self {
            command,
            stream,
            expected,
            first,
            rest: Vec::new(),
            state: None,
            outbox: Vec::new(),
        }
    }

    #[must_use]
    pub fn event(
        mut self,
        next: NewEvent,
    ) -> Self {
        self.rest.push(next);
        self
    }

    #[must_use]
    pub fn state(
        mut self,
        state: Payload,
    ) -> Self {
        self.state = Some(state);
        self
    }

    #[must_use]
    pub fn outbox(
        mut self,
        message: OutboxMessage,
    ) -> Self {
        self.outbox.push(message);
        self
    }

    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    #[must_use]
    pub const fn expected(&self) -> Expected {
        self.expected
    }
}

/// One message an append publishes for a relay to deliver after the commit is durable.
#[derive(Debug)]
pub struct OutboxMessage {
    pub(crate) topic: String,
    pub(crate) payload: Payload,
}

impl OutboxMessage {
    #[must_use]
    pub fn new(
        topic: impl Into<String>,
        payload: Payload,
    ) -> Self {
        Self {
            topic: topic.into(),
            payload,
        }
    }

    #[must_use]
    pub fn topic(&self) -> &str {
        &self.topic
    }

    #[must_use]
    pub const fn payload(&self) -> &Payload {
        &self.payload
    }
}

/// The store-assigned identity of one outbox row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutboxId(NonZeroU64);

impl OutboxId {
    pub(crate) const fn from_nonzero(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A pending outbox row as a relay reads it.
#[derive(Debug)]
pub struct OutboxEntry {
    id: OutboxId,
    topic: String,
    payload: Payload,
    at: SystemTime,
}

impl OutboxEntry {
    pub(crate) const fn new(
        id: OutboxId,
        topic: String,
        payload: Payload,
        at: SystemTime,
    ) -> Self {
        Self {
            id,
            topic,
            payload,
            at,
        }
    }

    #[must_use]
    pub const fn id(&self) -> OutboxId {
        self.id
    }

    #[must_use]
    pub fn topic(&self) -> &str {
        &self.topic
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

/// The commit an append landed as.
#[derive(Debug)]
pub struct Committed {
    position: CommitPosition,
    version: Version,
    events: Vec<Event>,
}

impl Committed {
    pub(crate) const fn new(
        position: CommitPosition,
        version: Version,
        events: Vec<Event>,
    ) -> Self {
        Self {
            position,
            version,
            events,
        }
    }

    #[must_use]
    pub const fn position(&self) -> CommitPosition {
        self.position
    }

    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    /// The events this commit wrote, in stream-version order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }
}

#[derive(Debug)]
pub enum Outcome {
    Committed(Committed),
    /// The command had already run; these are its original rows.
    Replayed(Committed),
    /// The version check refused the append; nothing was written.
    Conflicted {
        expected: Expected,
        found: Version,
    },
}

#[cfg(test)]
mod tests {
    use super::Append;
    use crate::event::{Kind, NewEvent};
    use crate::model::{CommandId, Expected, StreamId, Version};
    use crate::payload::Payload;

    fn event(index: usize) -> NewEvent {
        NewEvent::new(
            Kind::new(format!("turn.{index}")).unwrap(),
            Payload::json(&index).unwrap(),
        )
    }

    #[test]
    fn the_builder_keeps_the_first_event_and_the_expectation_it_was_given() {
        let stream = StreamId::new();
        let append = Append::new(
            CommandId::new(),
            stream,
            Expected::At(Version::new(2)),
            event(0),
        )
        .event(event(1))
        .event(event(2));

        assert_eq!(append.stream(), stream);
        assert_eq!(append.expected(), Expected::At(Version::new(2)));
        assert_eq!(append.rest.len(), 2);
        assert_eq!(append.first.kind.as_str(), "turn.0");
        assert_eq!(append.rest[1].kind.as_str(), "turn.2");
    }
}

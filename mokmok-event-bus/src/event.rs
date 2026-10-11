use std::sync::Arc;
use std::time::SystemTime;

use mokmok_protocol::EventID;

use crate::payload::Payload;
use crate::topic::Topic;

#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn offset_from(
        self,
        base: Self,
    ) -> Option<usize> {
        usize::try_from(self.0.checked_sub(base.0)?).ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct LogEpoch(uuid::Uuid);

impl LogEpoch {
    pub(crate) const fn new(value: uuid::Uuid) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Cursor {
    epoch: LogEpoch,
    seq: Seq,
}

impl Cursor {
    #[must_use]
    pub const fn new(
        epoch: LogEpoch,
        seq: Seq,
    ) -> Self {
        Self { epoch, seq }
    }

    #[must_use]
    pub const fn epoch(self) -> LogEpoch {
        self.epoch
    }

    #[must_use]
    pub const fn seq(self) -> Seq {
        self.seq
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    Beginning,
    After(Cursor),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Window {
    epoch: LogEpoch,
    first: Seq,
    next: Seq,
}

impl Window {
    pub(crate) const fn new(
        epoch: LogEpoch,
        first: Seq,
        next: Seq,
    ) -> Self {
        Self { epoch, first, next }
    }

    #[must_use]
    pub const fn epoch(self) -> LogEpoch {
        self.epoch
    }

    #[must_use]
    pub const fn first(self) -> Seq {
        self.first
    }

    #[must_use]
    pub const fn next(self) -> Seq {
        self.next
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Lapsed {
    held: Cursor,
    from: Seq,
}

impl Lapsed {
    pub(crate) const fn new(
        held: Cursor,
        from: Seq,
    ) -> Self {
        Self { held, from }
    }

    #[must_use]
    pub const fn held(self) -> Cursor {
        self.held
    }

    #[must_use]
    pub const fn from(self) -> Seq {
        self.from
    }

    #[must_use]
    pub const fn missed(self) -> u64 {
        self.from.get() - self.held.seq().get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    Unresumable { window: Window },
    TopicLimit,
    ShuttingDown,
}

impl std::fmt::Display for Refusal {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Unresumable { window } => write!(
                formatter,
                "the log holds {}..{} and cannot be read from that cursor",
                window.first().get(),
                window.next().get(),
            ),
            Self::TopicLimit => formatter.write_str("the bus has no room for another topic"),
            Self::ShuttingDown => formatter.write_str("the bus is shutting down"),
        }
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Event {
    id: EventID,
    topic: Topic,
    seq: Seq,
    at: SystemTime,
    payload: Payload,
}

impl Event {
    pub(crate) const fn new(
        id: EventID,
        topic: Topic,
        seq: Seq,
        at: SystemTime,
        payload: Payload,
    ) -> Self {
        Self {
            id,
            topic,
            seq,
            at,
            payload,
        }
    }

    #[must_use]
    pub const fn id(&self) -> EventID {
        self.id
    }

    #[must_use]
    pub const fn topic(&self) -> Topic {
        self.topic
    }

    #[must_use]
    pub const fn seq(&self) -> Seq {
        self.seq
    }

    #[must_use]
    pub const fn at(&self) -> SystemTime {
        self.at
    }

    #[must_use]
    pub const fn payload(&self) -> &Payload {
        &self.payload
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Published {
    id: EventID,
    seq: Seq,
    at: SystemTime,
}

impl Published {
    pub(crate) const fn new(
        id: EventID,
        seq: Seq,
        at: SystemTime,
    ) -> Self {
        Self { id, seq, at }
    }

    #[must_use]
    pub const fn id(self) -> EventID {
        self.id
    }

    #[must_use]
    pub const fn seq(self) -> Seq {
        self.seq
    }

    #[must_use]
    pub const fn at(self) -> SystemTime {
        self.at
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Batch {
    window: Window,
    lapsed: Option<Lapsed>,
    events: Vec<Arc<Event>>,
}

impl Batch {
    pub(crate) const fn new(
        window: Window,
        lapsed: Option<Lapsed>,
        events: Vec<Arc<Event>>,
    ) -> Self {
        Self {
            window,
            lapsed,
            events,
        }
    }

    #[must_use]
    pub const fn window(&self) -> Window {
        self.window
    }

    #[must_use]
    pub const fn lapsed(&self) -> Option<Lapsed> {
        self.lapsed
    }

    #[must_use]
    pub fn events(&self) -> &[Arc<Event>] {
        &self.events
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub fn after(&self) -> Position {
        let next = match (self.events.last(), self.lapsed) {
            (Some(event), _) => Seq::new(event.seq().get() + 1),
            (None, Some(lapsed)) => lapsed.from(),
            (None, None) => self.window.next(),
        };

        Position::After(Cursor::new(self.window.epoch(), next))
    }
}

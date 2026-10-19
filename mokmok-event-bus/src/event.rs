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
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct LogEpoch(uuid::Uuid);

impl LogEpoch {
    pub(crate) const fn new(value: uuid::Uuid) -> Self {
        Self(value)
    }

    pub(crate) const fn as_uuid(self) -> uuid::Uuid {
        self.0
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
        // Total rather than exact: a frame from a peer could carry a lapse that never happened.
        self.from.get().saturating_sub(self.held.seq().get())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    Unresumable { window: Window },
    TopicLimit,
    ConnectionLimit,
    ShuttingDown,
    UnknownSubscription,
    PullInFlight,
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
            Self::ConnectionLimit => formatter.write_str("the bus is at its connection ceiling"),
            Self::ShuttingDown => formatter.write_str("the bus is shutting down"),
            Self::UnknownSubscription => {
                formatter.write_str("no subscription with that id on this connection")
            },
            Self::PullInFlight => {
                formatter.write_str("a pull is already waiting on that subscription")
            },
        }
    }
}

impl Refusal {
    #[must_use]
    pub(crate) const fn upgrade_body(self) -> Option<&'static str> {
        match self {
            Self::ConnectionLimit => Some("connection_limit"),
            Self::ShuttingDown => Some("shutting_down"),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn from_upgrade_body(body: &str) -> Option<Self> {
        [Self::ConnectionLimit, Self::ShuttingDown]
            .into_iter()
            .find(|reason| reason.upgrade_body() == Some(body))
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

    /// The position after everything this batch carries, which is what a caller acks once the
    /// effects of the whole batch are durable.
    #[must_use]
    pub fn after(&self) -> Cursor {
        let next = match (self.events.last(), self.lapsed) {
            (Some(event), _) => Seq::new(event.seq().get() + 1),
            (None, Some(lapsed)) => lapsed.from(),
            (None, None) => self.window.next(),
        };

        Cursor::new(self.window.epoch(), next)
    }

    /// The position after one item of this batch, for a caller that commits per item.
    #[must_use]
    pub const fn after_event(
        &self,
        event: &Event,
    ) -> Cursor {
        Cursor::new(self.window.epoch(), Seq::new(event.seq().get() + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::{Batch, Event, LogEpoch, Refusal, Seq, Window};
    use crate::payload::Payload;
    use crate::topic::Topic;
    use mokmok_protocol::EventID;
    use proptest::prelude::*;
    use std::sync::Arc;
    use std::time::SystemTime;
    use uuid::Uuid;

    fn event(seq: u64) -> Arc<Event> {
        Arc::new(Event::new(
            EventID::from_uuid(Uuid::new_v4()),
            Topic::Global,
            Seq::new(seq),
            SystemTime::UNIX_EPOCH,
            Payload::json(&0).unwrap(),
        ))
    }

    #[test]
    fn an_upgrade_body_names_the_refusal_it_stands_for() {
        assert_eq!(
            Refusal::from_upgrade_body("connection_limit"),
            Some(Refusal::ConnectionLimit),
        );
        assert_eq!(
            Refusal::from_upgrade_body("shutting_down"),
            Some(Refusal::ShuttingDown),
        );
        assert_eq!(Refusal::from_upgrade_body("something else"), None);
    }

    proptest! {
        #[test]
        fn a_batch_reports_the_position_it_covers(
            first in 0_u64..=8,
            count in 0_usize..=8,
        ) {
            let epoch = LogEpoch::new(Uuid::new_v4());
            let events: Vec<Arc<Event>> =
                (0..count).map(|index| event(first + index as u64)).collect();
            let window = Window::new(epoch, Seq::new(first), Seq::new(first + count as u64));
            let batch = Batch::new(window, None, events);

            prop_assert_eq!(batch.is_empty(), count == 0);
            if count > 0 {
                let last = batch.events().last().unwrap();
                prop_assert_eq!(batch.after_event(last), batch.after());

                for pair in batch.events().windows(2) {
                    prop_assert!(
                        batch.after_event(&pair[0]).seq() < batch.after_event(&pair[1]).seq(),
                    );
                }
                for item in batch.events() {
                    prop_assert!(batch.after().seq() >= batch.after_event(item).seq());
                }
            }
        }
    }
}

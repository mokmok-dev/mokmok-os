use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::SystemTime;

use mokmok_protocol::EventID;

use crate::event::{Cursor, Event, Lapsed, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;

/// A subscription id minted by the server and echoed on every pull/ack.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct SubId(u64);

impl SubId {
    #[must_use]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Publish {
        req_id: u64,
        topic: Topic,
        payload: Payload,
    },
    Subscribe {
        req_id: u64,
        topic: Topic,
        subscriber: SubscriberId,
    },
    Pull {
        req_id: u64,
        sub: SubId,
        max: NonZeroUsize,
    },
    Ack {
        req_id: u64,
        sub: SubId,
        through: Cursor,
    },
}

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Response {
    Published {
        req_id: u64,
        id: EventID,
        seq: Seq,
        at: SystemTime,
    },
    Subscribed {
        req_id: u64,
        sub: SubId,
        window: Window,
    },
    Batch {
        req_id: u64,
        sub: SubId,
        window: Window,
        lapsed: Option<Lapsed>,
        events: Vec<Arc<Event>>,
    },
    Acked {
        req_id: u64,
        sub: SubId,
        through: Cursor,
    },
    Refused {
        req_id: u64,
        reason: Refusal,
    },
}

impl Response {
    #[must_use]
    pub(crate) const fn req_id(&self) -> u64 {
        match *self {
            Self::Published { req_id, .. }
            | Self::Subscribed { req_id, .. }
            | Self::Batch { req_id, .. }
            | Self::Acked { req_id, .. }
            | Self::Refused { req_id, .. } => req_id,
        }
    }
}

/// The operations a client may name. Separate from [`ResponseOp`] so a client cannot name a
/// response operation.
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOp {
    Publish,
    Subscribe,
    Pull,
    Ack,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseOp {
    Published,
    Subscribed,
    Batch,
    Acked,
    Refused,
}

/// A parsed client frame. Derived rather than an internally tagged enum because a struct reads
/// each field straight from the map: an enum buffers the frame into `serde`'s content first, and
/// the payload's `RawValue` bytes cannot survive that buffer.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    op: RequestOp,
    req_id: u64,
    #[serde(default)]
    topic: Option<Topic>,
    #[serde(default)]
    payload: Option<Payload>,
    #[serde(default)]
    sub: Option<SubId>,
    #[serde(default)]
    max: Option<NonZeroUsize>,
    #[serde(default)]
    through: Option<Cursor>,
    #[serde(default)]
    subscriber: Option<SubscriberId>,
}

/// A parsed server frame. Unknown fields stay allowed so a newer server can add one without
/// breaking an older client.
#[derive(serde::Deserialize)]
pub struct ResponseFrame {
    op: ResponseOp,
    req_id: u64,
    #[serde(default)]
    sub: Option<SubId>,
    #[serde(default)]
    window: Option<Window>,
    #[serde(default)]
    id: Option<EventID>,
    #[serde(default)]
    seq: Option<Seq>,
    #[serde(default)]
    at: Option<SystemTime>,
    #[serde(default)]
    lapsed: Option<Lapsed>,
    #[serde(default)]
    events: Option<Vec<Arc<Event>>>,
    #[serde(default)]
    through: Option<Cursor>,
    #[serde(default)]
    reason: Option<Refusal>,
}

#[derive(Debug, thiserror::Error)]
#[error("the frame does not match the operation it names")]
pub struct InvalidFrame;

impl RequestFrame {
    pub fn decode(text: &str) -> Option<Request> {
        let frame: Self = serde_json::from_str(text).ok()?;

        Request::try_from(frame).ok()
    }
}

impl ResponseFrame {
    pub fn decode(text: &str) -> Option<Response> {
        let frame: Self = serde_json::from_str(text).ok()?;

        Response::try_from(frame).ok()
    }
}

impl TryFrom<RequestFrame> for Request {
    type Error = InvalidFrame;

    fn try_from(frame: RequestFrame) -> Result<Self, Self::Error> {
        let RequestFrame {
            op,
            req_id,
            topic,
            payload,
            sub,
            max,
            through,
            subscriber,
        } = frame;

        match (op, topic, payload, sub, max, through, subscriber) {
            (RequestOp::Publish, Some(topic), Some(payload), None, None, None, None) => {
                Ok(Self::Publish {
                    req_id,
                    topic,
                    payload,
                })
            },
            (RequestOp::Subscribe, Some(topic), None, None, None, None, Some(subscriber)) => {
                Ok(Self::Subscribe {
                    req_id,
                    topic,
                    subscriber,
                })
            },
            (RequestOp::Pull, None, None, Some(sub), Some(max), None, None) => {
                Ok(Self::Pull { req_id, sub, max })
            },
            (RequestOp::Ack, None, None, Some(sub), None, Some(through), None) => Ok(Self::Ack {
                req_id,
                sub,
                through,
            }),
            _ => Err(InvalidFrame),
        }
    }
}

impl TryFrom<ResponseFrame> for Response {
    type Error = InvalidFrame;

    fn try_from(frame: ResponseFrame) -> Result<Self, Self::Error> {
        let ResponseFrame {
            op,
            req_id,
            sub,
            window,
            id,
            seq,
            at,
            lapsed,
            events,
            through,
            reason,
        } = frame;

        match (
            op, sub, window, id, seq, at, lapsed, events, through, reason,
        ) {
            (
                ResponseOp::Published,
                None,
                None,
                Some(id),
                Some(seq),
                Some(at),
                None,
                None,
                None,
                None,
            ) => Ok(Self::Published {
                req_id,
                id,
                seq,
                at,
            }),
            (
                ResponseOp::Subscribed,
                Some(sub),
                Some(window),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ) => Ok(Self::Subscribed {
                req_id,
                sub,
                window,
            }),
            (
                ResponseOp::Batch,
                Some(sub),
                Some(window),
                None,
                None,
                None,
                lapsed,
                Some(events),
                None,
                None,
            ) => Ok(Self::Batch {
                req_id,
                sub,
                window,
                lapsed,
                events,
            }),
            (
                ResponseOp::Acked,
                Some(sub),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(through),
                None,
            ) => Ok(Self::Acked {
                req_id,
                sub,
                through,
            }),
            (ResponseOp::Refused, _, None, None, None, None, None, None, None, Some(reason)) => {
                Ok(Self::Refused { req_id, reason })
            },
            _ => Err(InvalidFrame),
        }
    }
}

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::SystemTime;

use mokmok_protocol::EventID;

use crate::event::{Batch, Event, Lapsed, Position, Published, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::topic::Topic;

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Open {
        topic: Topic,
    },
    Publish {
        payload: Payload,
    },
    Pull {
        position: Position,
        max: NonZeroUsize,
    },
}

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Response {
    Opened {
        window: Window,
    },
    Published {
        id: EventID,
        seq: Seq,
        at: SystemTime,
    },
    Batch {
        window: Window,
        lapsed: Option<Lapsed>,
        events: Vec<Arc<Event>>,
    },
    Refused {
        reason: Refusal,
    },
}

impl From<Batch> for Response {
    fn from(batch: Batch) -> Self {
        Self::Batch {
            window: batch.window(),
            lapsed: batch.lapsed(),
            events: batch.events().to_vec(),
        }
    }
}

impl From<Published> for Response {
    fn from(published: Published) -> Self {
        Self::Published {
            id: published.id(),
            seq: published.seq(),
            at: published.at(),
        }
    }
}

/// The operations a client may name. Separate from [`ResponseOp`] so a client cannot name a
/// response operation.
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOp {
    Open,
    Publish,
    Pull,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseOp {
    Opened,
    Published,
    Batch,
    Refused,
}

/// A parsed client frame. Derived rather than an internally tagged enum because a struct reads
/// each field straight from the map: an enum buffers the frame into `serde`'s content first, and
/// the payload's `RawValue` bytes cannot survive that buffer.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    op: RequestOp,
    #[serde(default)]
    topic: Option<Topic>,
    #[serde(default)]
    payload: Option<Payload>,
    #[serde(default)]
    position: Option<Position>,
    #[serde(default)]
    max: Option<NonZeroUsize>,
}

/// A parsed server frame. Unknown fields stay allowed so a newer server can add one without
/// breaking an older client.
#[derive(serde::Deserialize)]
pub struct ResponseFrame {
    op: ResponseOp,
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
        match (
            frame.op,
            frame.topic,
            frame.payload,
            frame.position,
            frame.max,
        ) {
            (RequestOp::Open, Some(topic), None, None, None) => Ok(Self::Open { topic }),
            (RequestOp::Publish, None, Some(payload), None, None) => Ok(Self::Publish { payload }),
            (RequestOp::Pull, None, None, Some(position), Some(max)) => {
                Ok(Self::Pull { position, max })
            },
            _ => Err(InvalidFrame),
        }
    }
}

impl TryFrom<ResponseFrame> for Response {
    type Error = InvalidFrame;

    fn try_from(frame: ResponseFrame) -> Result<Self, Self::Error> {
        match (
            frame.op,
            frame.window,
            frame.id,
            frame.seq,
            frame.at,
            frame.lapsed,
            frame.events,
            frame.reason,
        ) {
            (ResponseOp::Opened, Some(window), None, None, None, None, None, None) => {
                Ok(Self::Opened { window })
            },
            (ResponseOp::Published, None, Some(id), Some(seq), Some(at), None, None, None) => {
                Ok(Self::Published { id, seq, at })
            },
            (ResponseOp::Batch, Some(window), None, None, None, lapsed, Some(events), None) => {
                Ok(Self::Batch {
                    window,
                    lapsed,
                    events,
                })
            },
            (ResponseOp::Refused, None, None, None, None, None, None, Some(reason)) => {
                Ok(Self::Refused { reason })
            },
            _ => Err(InvalidFrame),
        }
    }
}

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::SystemTime;

use mokmok_protocol::EventID;
use serde::de::{Error as _, MapAccess, Visitor};

use crate::event::{Batch, Event, Lapsed, Position, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::topic::Topic;

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
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

/// The fields a request frame may carry, for `unknown_field` diagnostics.
const REQUEST_FIELDS: &[&str] = &["op", "topic", "payload", "position", "max"];

/// The operations a request frame may name.
const REQUEST_OPS: &[&str] = &["open", "publish", "pull"];

/// The fields a response frame may carry, for `unknown_field` diagnostics.
const RESPONSE_FIELDS: &[&str] = &[
    "op", "window", "id", "seq", "at", "lapsed", "events", "reason",
];

/// The operations a response frame may name.
const RESPONSE_OPS: &[&str] = &["opened", "published", "batch", "refused"];

/// Rejects a field the named operation does not carry, which is the refusal
/// `deny_unknown_fields` gives a derived frame.
fn refuse<E: serde::de::Error>(
    fields: &'static [&'static str],
    present: &[(&'static str, bool)],
    allowed: &[&str],
) -> Result<(), E> {
    for (name, is_present) in present {
        if *is_present && !allowed.contains(name) {
            return Err(E::unknown_field(name, fields));
        }
    }

    Ok(())
}

struct RequestParts {
    topic: Option<Topic>,
    payload: Option<Payload>,
    position: Option<Position>,
    max: Option<NonZeroUsize>,
}

impl RequestParts {
    fn refuse<E: serde::de::Error>(
        &self,
        allowed: &[&str],
    ) -> Result<(), E> {
        refuse::<E>(
            REQUEST_FIELDS,
            &[
                ("topic", self.topic.is_some()),
                ("payload", self.payload.is_some()),
                ("position", self.position.is_some()),
                ("max", self.max.is_some()),
            ],
            allowed,
        )
    }
}

// Deserialized by hand rather than derived: an internally tagged enum buffers the whole frame
// into `serde`'s `Content` before it knows the variant, and `RawValue` cannot be recovered from
// that buffer, so a derived frame would rewrite every payload it carried.
impl<'de> serde::Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RequestVisitor;

        impl<'de> Visitor<'de> for RequestVisitor {
            type Value = Request;

            fn expecting(
                &self,
                formatter: &mut fmt::Formatter<'_>,
            ) -> fmt::Result {
                formatter.write_str("a request frame")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Request, A::Error> {
                let mut op: Option<String> = None;
                let mut parts = RequestParts {
                    topic: None,
                    payload: None,
                    position: None,
                    max: None,
                };

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "op" => op = Some(map.next_value()?),
                        "topic" => parts.topic = Some(map.next_value()?),
                        "payload" => parts.payload = Some(map.next_value()?),
                        "position" => parts.position = Some(map.next_value()?),
                        "max" => parts.max = Some(map.next_value()?),
                        other => return Err(A::Error::unknown_field(other, REQUEST_FIELDS)),
                    }
                }

                match op.as_deref() {
                    Some("open") => {
                        parts.refuse(&["topic"])?;

                        Ok(Request::Open {
                            topic: parts
                                .topic
                                .ok_or_else(|| A::Error::missing_field("topic"))?,
                        })
                    },
                    Some("publish") => {
                        parts.refuse(&["payload"])?;

                        Ok(Request::Publish {
                            payload: parts
                                .payload
                                .ok_or_else(|| A::Error::missing_field("payload"))?,
                        })
                    },
                    Some("pull") => {
                        parts.refuse(&["position", "max"])?;

                        Ok(Request::Pull {
                            position: parts
                                .position
                                .ok_or_else(|| A::Error::missing_field("position"))?,
                            max: parts.max.ok_or_else(|| A::Error::missing_field("max"))?,
                        })
                    },
                    Some(other) => Err(A::Error::unknown_variant(other, REQUEST_OPS)),
                    None => Err(A::Error::missing_field("op")),
                }
            }
        }

        deserializer.deserialize_map(RequestVisitor)
    }
}

struct ResponseParts {
    window: Option<Window>,
    id: Option<EventID>,
    seq: Option<Seq>,
    at: Option<SystemTime>,
    lapsed: Option<Lapsed>,
    /// `lapsed` is optional, so its presence needs tracking of its own: a frame carrying a null
    /// `lapsed` still carries a field the other variants may not have.
    lapsed_present: bool,
    events: Option<Vec<Arc<Event>>>,
    reason: Option<Refusal>,
}

impl ResponseParts {
    fn refuse<E: serde::de::Error>(
        &self,
        allowed: &[&str],
    ) -> Result<(), E> {
        refuse::<E>(
            RESPONSE_FIELDS,
            &[
                ("window", self.window.is_some()),
                ("id", self.id.is_some()),
                ("seq", self.seq.is_some()),
                ("at", self.at.is_some()),
                ("lapsed", self.lapsed_present),
                ("events", self.events.is_some()),
                ("reason", self.reason.is_some()),
            ],
            allowed,
        )
    }
}

impl<'de> serde::Deserialize<'de> for Response {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ResponseVisitor;

        impl<'de> Visitor<'de> for ResponseVisitor {
            type Value = Response;

            fn expecting(
                &self,
                formatter: &mut fmt::Formatter<'_>,
            ) -> fmt::Result {
                formatter.write_str("a response frame")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Response, A::Error> {
                let mut op: Option<String> = None;
                let mut parts = ResponseParts {
                    window: None,
                    id: None,
                    seq: None,
                    at: None,
                    lapsed: None,
                    lapsed_present: false,
                    events: None,
                    reason: None,
                };

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "op" => op = Some(map.next_value()?),
                        "window" => parts.window = Some(map.next_value()?),
                        "id" => parts.id = Some(map.next_value()?),
                        "seq" => parts.seq = Some(map.next_value()?),
                        "at" => parts.at = Some(map.next_value()?),
                        "lapsed" => {
                            parts.lapsed_present = true;
                            parts.lapsed = map.next_value::<Option<Lapsed>>()?;
                        },
                        "events" => parts.events = Some(map.next_value()?),
                        "reason" => parts.reason = Some(map.next_value()?),
                        other => return Err(A::Error::unknown_field(other, RESPONSE_FIELDS)),
                    }
                }

                match op.as_deref() {
                    Some("opened") => {
                        parts.refuse(&["window"])?;

                        Ok(Response::Opened {
                            window: parts
                                .window
                                .ok_or_else(|| A::Error::missing_field("window"))?,
                        })
                    },
                    Some("published") => {
                        parts.refuse(&["id", "seq", "at"])?;

                        Ok(Response::Published {
                            id: parts.id.ok_or_else(|| A::Error::missing_field("id"))?,
                            seq: parts.seq.ok_or_else(|| A::Error::missing_field("seq"))?,
                            at: parts.at.ok_or_else(|| A::Error::missing_field("at"))?,
                        })
                    },
                    Some("batch") => {
                        parts.refuse(&["window", "lapsed", "events"])?;

                        Ok(Response::Batch {
                            window: parts
                                .window
                                .ok_or_else(|| A::Error::missing_field("window"))?,
                            lapsed: parts.lapsed,
                            events: parts
                                .events
                                .ok_or_else(|| A::Error::missing_field("events"))?,
                        })
                    },
                    Some("refused") => {
                        parts.refuse(&["reason"])?;

                        Ok(Response::Refused {
                            reason: parts
                                .reason
                                .ok_or_else(|| A::Error::missing_field("reason"))?,
                        })
                    },
                    Some(other) => Err(A::Error::unknown_variant(other, RESPONSE_OPS)),
                    None => Err(A::Error::missing_field("op")),
                }
            }
        }

        deserializer.deserialize_map(ResponseVisitor)
    }
}

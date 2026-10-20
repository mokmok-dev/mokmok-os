//! The messages a terminal and a daemon exchange over the event bus.
//!
//! The bus carries opaque JSON, so every idempotence and routing rule lives in these payloads.
//! One `SessionCommit` is one ledger transaction: the records it carries are exactly the records
//! the fold replays, which is what keeps a consumer's fold equal to the daemon's transcript.
//!
//! The crate is an adapter: it names the bus's [`Topic`] and the store's `Payload`, and it owns the
//! payload vocabulary both ends decode.

use std::path::PathBuf;

use mokmok_agent_runtime::{Record, StepId, StepKey, TaskRecord};
use mokmok_event_bus::Topic;
use mokmok_event_store::{OutboxMessage, Payload, PayloadError};
use mokmok_protocol::{AgentID, CommandID, ExecutionID, Moment, SessionID, TurnID};
use mokmok_state_machine::{Phase, Resolution};

pub mod chunk;
pub mod topic;

pub use chunk::{HISTORY_BYTES, ROSTER_BYTES, history_chunks, roster_chunks};
pub use topic::{command_topic, daemon_subscriber, daemon_topic, daemon_view_subscriber};
pub use topic::{session_subscriber, session_topic};

/// The wire version both ends must name. A daemon answers a newer client with
/// [`Refusal::Unsupported`] rather than guessing at a shape it does not know.
pub const PROTOCOL: u16 = 1;

/// A client's intent, published on [`command_topic`].
///
/// `request` is the identity of the whole command. A `Speak` also derives the ledger step from it,
/// so a redelivered command replays the durable write instead of opening a second turn.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    Hello {
        request: CommandID,
        protocol: u16,
    },
    List {
        request: CommandID,
    },
    Create {
        request: CommandID,
        session: SessionID,
        goal: String,
        checkout: PathBuf,
    },
    Open {
        request: CommandID,
        session: SessionID,
    },
    Speak {
        request: CommandID,
        session: SessionID,
        line: String,
    },
    History {
        request: CommandID,
        session: SessionID,
    },
}

impl Command {
    /// The identity a retry of this command reuses.
    #[must_use]
    pub const fn request(&self) -> CommandID {
        match self {
            Self::Hello { request, .. }
            | Self::List { request }
            | Self::Create { request, .. }
            | Self::Open { request, .. }
            | Self::Speak { request, .. }
            | Self::History { request, .. } => *request,
        }
    }
}

/// A daemon's answer to a command, published on [`command_topic`].
///
/// The record stream is the truth for a spoken line; this reply is the evidence a command with no
/// record (a seed, a refusal, a roster request) was seen.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Welcome {
        request: CommandID,
        daemon: AgentID,
        protocol: u16,
    },
    Ack {
        request: CommandID,
        outcome: Outcome,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Accepted { session: SessionID },
    Refused { reason: Refusal },
}

/// A refusal the daemon decided at its boundary. It names the command's subject and nothing about
/// how the daemon enforces it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    #[error("the daemon holds no session {session:?}")]
    UnknownSession { session: SessionID },
    #[error("session {session:?} recorded no task")]
    Untasked { session: SessionID },
    #[error("session {session:?} recorded a different goal")]
    GoalDiffers { session: SessionID },
    #[error("checkout {checkout:?} is outside the daemon's write grants")]
    CheckoutUngranted { checkout: PathBuf },
    #[error("session {session:?} is not waiting for a line")]
    NotAwaiting { session: SessionID },
    #[error("session {session:?} is closed")]
    Closed { session: SessionID },
    #[error("the line is {found} bytes and the limit is {limit}")]
    TooMuch { limit: usize, found: usize },
    #[error("the command is malformed: {detail}")]
    Malformed { detail: String },
    #[error("the daemon cannot open a session: {detail}")]
    Unavailable { detail: String },
    #[error("the daemon speaks protocol {protocol} and this client speaks another")]
    Unsupported { protocol: u16 },
}

/// Everything the daemon publishes on one session's topic.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    Commit(SessionCommit),
    History(HistoryChunk),
    /// The daemon could not answer a history request, so the reader stops waiting for chunks.
    HistoryRefused {
        request: CommandID,
        session: SessionID,
        reason: Refusal,
    },
    /// A created session's task, before any record exists.
    ///
    /// Non-durable: the first spoken line commits the same task through the runtime's `Begin`
    /// move. A reconnect reads it back from [`HistoryChunk::seed`] instead.
    Seeded {
        session: SessionID,
        task: TaskRecord,
    },
    /// The daemon stopped a turn for a reason no record carries, such as a provider failure it
    /// could not settle.
    Trouble {
        session: SessionID,
        detail: String,
    },
}

/// One ledger transaction, exactly as the fold will replay it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SessionCommit {
    pub session: SessionID,
    /// The ledger's command identity: a runtime step's [`StepId`], or the id a slice append named.
    pub command: CommandID,
    /// The routing header: which turn and execution this transaction touched.
    pub notice: Notice,
    /// The records `Log::ordered` gained from this transaction, in log order.
    pub records: Vec<Record>,
}

impl SessionCommit {
    /// The outbox row the relay publishes once the transaction that wrote it committed.
    ///
    /// # Errors
    ///
    /// Returns a [`PayloadError`] when the commit cannot be encoded.
    pub fn outbox(&self) -> Result<OutboxMessage, PayloadError> {
        Ok(OutboxMessage::new(
            Topic::Session(self.session).to_string(),
            Payload::json(&SessionEvent::Commit(self.clone()))?,
        ))
    }
}

/// The identities one transaction touched, without the content.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Notice {
    pub turn: Option<TurnID>,
    pub execution: Option<ExecutionID>,
    pub resolution: Option<Resolution>,
    pub at: Moment,
}

/// One replacement of a session's history, split to stay inside the bus frame.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryChunk {
    pub request: CommandID,
    pub session: SessionID,
    pub index: u32,
    pub last: bool,
    /// The task a created session has but its log does not yet hold.
    pub seed: Option<TaskRecord>,
    pub records: Vec<Record>,
}

/// Everything the daemon publishes on its own topic.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    Roster(RosterChunk),
    /// Display-only prose a step is streaming. Best effort; the settled answer is a record.
    Progress {
        session: SessionID,
        text: String,
    },
}

/// One revision of the daemon's session listing, split to stay inside the bus frame.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RosterChunk {
    pub daemon: AgentID,
    pub revision: u64,
    pub index: u32,
    pub last: bool,
    pub entries: Vec<RosterEntry>,
}

/// One session as a listing shows it, before a client opens it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RosterEntry {
    pub session: SessionID,
    pub phase: Phase,
    pub goal: Option<String>,
    pub checkout: Option<PathBuf>,
    pub turns: u64,
}

/// The step id a spoken line derives, so a redelivered request replays its own write.
#[must_use]
pub fn speech_step(
    session: SessionID,
    request: CommandID,
) -> StepId {
    StepId::derive(session, StepKey::SpeechBy { request })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mokmok_event_store::Payload;
    use mokmok_protocol::{AgentID, CommandID, SessionID};
    use mokmok_state_machine::{Ending, Phase};
    use uuid::Uuid;

    use super::{
        Command, DaemonEvent, HistoryChunk, Notice, Outcome, Refusal, Reply, RosterChunk,
        RosterEntry, SessionCommit, SessionEvent,
    };

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(1))
    }

    fn request() -> CommandID {
        CommandID::from_uuid(Uuid::from_u128(2))
    }

    fn daemon() -> AgentID {
        AgentID::from_uuid(Uuid::from_u128(3))
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let payload = Payload::json(value).unwrap();

        payload.decode().unwrap()
    }

    #[test]
    fn every_command_round_trips() {
        for command in [
            Command::Hello {
                request: request(),
                protocol: super::PROTOCOL,
            },
            Command::List { request: request() },
            Command::Create {
                request: request(),
                session: session(),
                goal: "port the parser".to_owned(),
                checkout: PathBuf::from("/tmp/checkout"),
            },
            Command::Open {
                request: request(),
                session: session(),
            },
            Command::Speak {
                request: request(),
                session: session(),
                line: "hello".to_owned(),
            },
            Command::History {
                request: request(),
                session: session(),
            },
        ] {
            assert_eq!(round_trip(&command), command);
        }
    }

    #[test]
    fn a_command_names_the_request_a_retry_reuses() {
        let command = Command::Speak {
            request: request(),
            session: session(),
            line: "hello".to_owned(),
        };

        assert_eq!(command.request(), request());
    }

    #[test]
    fn a_reply_and_a_refusal_round_trip() {
        for reply in [
            Reply::Welcome {
                request: request(),
                daemon: daemon(),
                protocol: super::PROTOCOL,
            },
            Reply::Ack {
                request: request(),
                outcome: Outcome::Accepted { session: session() },
            },
            Reply::Ack {
                request: request(),
                outcome: Outcome::Refused {
                    reason: Refusal::CheckoutUngranted {
                        checkout: PathBuf::from("/etc"),
                    },
                },
            },
        ] {
            assert_eq!(round_trip(&reply), reply);
        }
    }

    #[test]
    fn a_session_event_round_trips() {
        let event = SessionEvent::History(HistoryChunk {
            request: request(),
            session: session(),
            index: 0,
            last: true,
            seed: None,
            records: Vec::new(),
        });

        assert_eq!(round_trip(&event), event);
    }

    #[test]
    fn a_session_commit_round_trips_and_builds_an_outbox_row() {
        let commit = SessionCommit {
            session: session(),
            command: request(),
            notice: Notice {
                turn: None,
                execution: None,
                resolution: None,
                at: mokmok_protocol::Moment::EPOCH,
            },
            records: Vec::new(),
        };

        assert_eq!(round_trip(&commit), commit);
        assert_eq!(
            commit
                .outbox()
                .unwrap()
                .payload()
                .decode::<SessionEvent>()
                .unwrap(),
            SessionEvent::Commit(commit),
        );
    }

    #[test]
    fn a_roster_round_trips() {
        let event = DaemonEvent::Roster(RosterChunk {
            daemon: daemon(),
            revision: 7,
            index: 0,
            last: true,
            entries: vec![RosterEntry {
                session: session(),
                phase: Phase::Closed(Ending::Completed),
                goal: Some("port the parser".to_owned()),
                checkout: Some(PathBuf::from("/tmp/checkout")),
                turns: 4,
            }],
        });

        assert_eq!(round_trip(&event), event);
    }

    #[test]
    fn a_phase_and_an_ending_keep_their_wire_names() {
        let payload = Payload::json(&Phase::Closed(Ending::Cancelled)).unwrap();

        assert_eq!(payload.as_str(), r#"{"closed":"cancelled"}"#);
        assert_eq!(
            payload.decode::<Phase>().unwrap(),
            Phase::Closed(Ending::Cancelled),
        );
    }
}

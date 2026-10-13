//! The runtime's durable vocabulary: one record per decision or outcome the loop commits.

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{Attempt, Fact, Rejection, TurnFact};

use crate::model::{Action, Usage};
use crate::task::Task;
use crate::tool::ToolReport;

/// Bytes outside the log, named by their content hash.
///
/// The reference is what the log keeps; the bytes live in the artifact store and are written
/// before the record that names them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ArtifactRef {
    pub hash: String,
    pub bytes: u64,
    /// Whether capture stopped before the producer stopped writing.
    pub truncated: bool,
}

/// The goal and the setting, written once and first.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TaskRecord {
    pub task: Task,
}

/// The exact context one attempt was handed.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PromptRecord {
    pub turn: TurnID,
    pub attempt: Attempt,
    pub context: ArtifactRef,
}

/// The action one attempt answered with, and what it cost.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct AnswerRecord {
    pub turn: TurnID,
    pub attempt: Attempt,
    pub action: Action,
    pub text: ArtifactRef,
    pub model: String,
    pub usage: Usage,
}

/// The command about to be dispatched.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CallRecord {
    pub turn: TurnID,
    pub execution: ExecutionID,
    pub command: String,
}

/// One durable record.
///
/// The machine facts and the result ride the same vocabulary as the runtime's own content records,
/// so one fold — [`Record::apply`](crate::Transcript) — validates a whole step. The kernel maps each
/// variant back to the entry kind that owns it, so the durable log keeps the kinds it already had
/// and gains exactly one for [`Record`] itself.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    /// A fact of the session machine.
    Session { fact: Fact },
    /// A fact of one turn's machine, with the turn it belongs to.
    Turn { turn: TurnID, fact: TurnFact },
    /// What one execution produced, with the bounded rendering of its outcome.
    Result {
        execution: ExecutionID,
        report: ToolReport,
    },
    /// The goal and the setting.
    Task(TaskRecord),
    /// The exact context one attempt was handed.
    Prompt(PromptRecord),
    /// The action one attempt answered with.
    Answer(AnswerRecord),
    /// The command about to be dispatched.
    Call(CallRecord),
}

/// The identity of one durable commitment.
///
/// A step's id is derived from the session and the move rather than minted per invocation, so two
/// invocations that reach the same move derive the same id and the second append replays.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct StepId(uuid::Uuid);

/// Which move a [`StepId`] names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepKey {
    /// The task, written once and first.
    Task,
    /// One turn's prompt, written before the model is called.
    AskIntent { turn: u64, attempt: u32 },
    /// One turn's answer, written after the model answered.
    AskOutcome { turn: u64, attempt: u32 },
    /// One turn's command, written before the tool is dispatched.
    ActIntent { turn: u64 },
    /// One turn's outcome, written after the tool ran.
    ActOutcome { turn: u64 },
    /// A turn's settlement, and the session's close when it carries an ending.
    ///
    /// [`Move::Close`](crate::Move) runs twice on one turn — once to settle it and once to close
    /// the session — so the closing step carries its own key; one id for both would make the
    /// second a replay of the first and the session would never close.
    Close { turn: u64, session: bool },
}

impl StepId {
    /// The id one move on one session always derives.
    ///
    /// The key is `{tag}:{n}` with the attempt appended for an `ask-*` tag, so a resumed invocation
    /// that reconstructs the same move reproduces the same id.
    #[must_use]
    pub fn derive(
        session: SessionID,
        key: StepKey,
    ) -> Self {
        Self(uuid::Uuid::new_v5(&session.as_uuid(), key.key().as_bytes()))
    }

    #[must_use]
    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl StepKey {
    fn key(self) -> String {
        match self {
            Self::Task => "task:0".to_owned(),
            Self::AskIntent { turn, attempt } => format!("ask-intent:{turn}:{attempt}"),
            Self::AskOutcome { turn, attempt } => format!("ask-outcome:{turn}:{attempt}"),
            Self::ActIntent { turn } => format!("act-intent:{turn}"),
            Self::ActOutcome { turn } => format!("act-outcome:{turn}"),
            Self::Close {
                turn,
                session: false,
            } => format!("close:{turn}"),
            Self::Close {
                turn,
                session: true,
            } => format!("close:{turn}:session"),
        }
    }
}

/// One transaction's worth of records, with the identity that makes a repeat a replay.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Step {
    pub id: StepId,
    pub session: SessionID,
    pub records: Vec<Record>,
}

impl Step {
    #[must_use]
    pub const fn new(
        id: StepId,
        session: SessionID,
        records: Vec<Record>,
    ) -> Self {
        Self {
            id,
            session,
            records,
        }
    }
}

/// What the journal did with a step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Receipt {
    /// The step was new and is now durable.
    Written,
    /// The step was already durable under its id.
    Replayed,
}

/// The key a content record is unique under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKey {
    Task,
    Prompt { turn: TurnID, attempt: Attempt },
    Answer { turn: TurnID, attempt: Attempt },
    Call(ExecutionID),
    Result(ExecutionID),
}

/// Why the fold refused a record.
///
/// A call carries the command in two records, its own and the answer's, so the fold checks the two
/// agree: [`Refusal::CommandMismatch`] is that disagreement, and it names both commands because
/// [`Refusal::Conflict`] means one key recorded twice, not two records that must agree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// A second record under one key carried different content.
    Conflict(RecordKey),
    /// A content record whose admitting machine fact the fold does not hold.
    Floating(RecordKey),
    /// A machine fact the fold refused.
    Rejected(Rejection),
    /// A call whose command the answer recorded for the same turn did not name.
    CommandMismatch {
        turn: TurnID,
        answer: String,
        call: String,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Conflict(key) => {
                write!(formatter, "{key} already recorded with different content")
            },
            Self::Floating(key) => write!(formatter, "no machine fact admits {key}"),
            Self::Rejected(rejection) => write!(formatter, "{rejection}"),
            Self::CommandMismatch { turn, answer, call } => write!(
                formatter,
                "the call for turn {} names {call:?} but its answer named {answer:?}",
                turn.as_uuid(),
            ),
        }
    }
}

impl std::error::Error for Refusal {}

impl std::fmt::Display for RecordKey {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Task => formatter.write_str("the task"),
            Self::Prompt { turn, attempt } => write!(
                formatter,
                "the prompt for turn {} attempt {}",
                turn.as_uuid(),
                attempt.get(),
            ),
            Self::Answer { turn, attempt } => write!(
                formatter,
                "the answer for turn {} attempt {}",
                turn.as_uuid(),
                attempt.get(),
            ),
            Self::Call(execution) => {
                write!(formatter, "the call for execution {}", execution.as_uuid())
            },
            Self::Result(execution) => {
                write!(formatter, "the result of execution {}", execution.as_uuid())
            },
        }
    }
}

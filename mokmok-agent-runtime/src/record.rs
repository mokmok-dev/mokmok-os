//! The runtime's durable vocabulary: one record per decision or outcome the loop commits.

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use mokmok_state_machine::{Attempt, Fact, Rejection, TurnFact};

use crate::edit::EditReport;
use crate::find::FindReport;
use crate::model::{Action, Usage};
use crate::task::Task;
use crate::tool::ToolReport;

/// How many bytes of one human line the log may hold.
///
/// The log is folded whole on every step and replayed on every launch, so a record stays bounded.
/// The runtime refuses a longer line rather than truncating it, because a truncated sentence is a
/// different sentence.
pub const MAX_SPEECH_BYTES: usize = 4 * 1024;

/// How many bytes of the model's query one find record may hold.
///
/// The log is folded whole on every step, so a record stays bounded. A query is clipped rather than
/// refused: it is a phrase the model repeats, and the words it opened with are the words it meant.
pub const MAX_QUERY_BYTES: usize = 512;

/// How many bytes of one patch the log may hold.
///
/// A clipped diff is a different diff, so an over-long patch is refused rather than clipped.
pub const MAX_PATCH_BYTES: usize = 32 * 1024;

/// The head of `text`, clipped to [`MAX_QUERY_BYTES`] without splitting a character.
///
/// The clip happens where the tool arguments are decoded, so the answer, the record, and the search
/// all name one string rather than three lengths of it.
#[must_use]
pub fn clip_query(text: &str) -> String {
    clipped(text, MAX_QUERY_BYTES)
}

/// Why a patch may not be recorded, or `None` when it may.
#[must_use]
pub fn patch_refusal(patch: &str) -> Option<String> {
    if patch.len() > MAX_PATCH_BYTES {
        return Some(format!(
            "the patch is {} bytes, and the log holds {}",
            patch.len(),
            MAX_PATCH_BYTES,
        ));
    }

    None
}

/// The head of `text`, clipped to [`MAX_PROSE_BYTES`] without splitting a character.
///
/// The provider clips what the model said, and so does the scripted model, because the cap is this
/// crate's to keep: a record holds what a prompt and a screen need, and the whole body stays in the
/// artifact store under the reference `AnswerRecord::text` names.
#[must_use]
pub fn clip_prose(text: &str) -> String {
    clipped(text, MAX_PROSE_BYTES)
}

#[must_use]
fn clipped(
    text: &str,
    limit: usize,
) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }

    text[..text.floor_char_boundary(limit)].to_owned()
}

/// How many bytes of the model's own words one answer record may hold.
///
/// The whole body stays in the artifact store, under the reference `AnswerRecord::text` names, so
/// the clip costs a reader nothing: the record holds only what the next prompt and a screen need.
pub const MAX_PROSE_BYTES: usize = 8 * 1024;

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
    /// The model's own words, clipped; empty when the provider had none to give.
    ///
    /// `default` keeps a log written before this field readable: an older answer folds with no prose
    /// rather than refusing to decode.
    #[serde(default)]
    pub prose: String,
    pub text: ArtifactRef,
    pub model: String,
    pub usage: Usage,
}

/// One human line, as the input that opened one turn.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SpeechRecord {
    pub turn: TurnID,
    pub text: String,
}

/// The command about to be dispatched.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CallRecord {
    pub turn: TurnID,
    pub execution: ExecutionID,
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FindRecord {
    pub turn: TurnID,
    pub query: String,
    pub report: FindReport,
}

/// One edit about to be written, and the bytes it will write.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct EditIntentRecord {
    pub turn: TurnID,
    pub execution: ExecutionID,
    /// The patch, exactly as the answer named it.
    pub patch: String,
    /// The sha256 the model believed the file held, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    /// The file, as the patch's own labels name it.
    pub relative: String,
    /// The bytes the file holds. Always a blob in the store.
    pub before: ArtifactRef,
    /// The bytes the patch produces. Always a blob in the store.
    pub after: ArtifactRef,
    /// Where each hunk landed.
    pub placement: String,
}

/// One edit's outcome.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct EditOutcomeRecord {
    pub turn: TurnID,
    pub execution: ExecutionID,
    pub report: EditReport,
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
    Session {
        fact: Fact,
    },
    /// A fact of one turn's machine, with the turn it belongs to.
    Turn {
        turn: TurnID,
        fact: TurnFact,
    },
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
    Find(FindRecord),
    /// An edit about to be written, with the bytes it will write.
    EditIntent(EditIntentRecord),
    /// An edit's outcome.
    EditOutcome(EditOutcomeRecord),
    /// A human's line, which opened this turn.
    Speech(SpeechRecord),
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
    /// One turn's edit intent, written before the effect it describes.
    EditIntent { turn: u64 },
    /// One turn's edit outcome, written after the effect.
    EditOutcome { turn: u64 },
    /// One turn's find, written with its settlement in the one step a find takes.
    Find { turn: u64 },
    /// A turn's settlement, and the session's close when it carries an ending.
    ///
    /// [`Move::Close`](crate::Move) runs twice on one turn — once to settle it and once to close
    /// the session — so the closing step carries its own key; one id for both would make the
    /// second a replay of the first and the session would never close.
    Close { turn: u64, session: bool },
    /// A human's line, written under the turn it opens.
    Speech { turn: u64 },
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
            Self::EditIntent { turn } => format!("edit-intent:{turn}"),
            Self::EditOutcome { turn } => format!("edit-outcome:{turn}"),
            Self::Find { turn } => format!("find:{turn}"),
            Self::Close {
                turn,
                session: false,
            } => format!("close:{turn}"),
            Self::Close {
                turn,
                session: true,
            } => format!("close:{turn}:session"),
            Self::Speech { turn } => format!("speech:{turn}"),
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
    Find(TurnID),
    EditIntent(ExecutionID),
    EditOutcome(ExecutionID),
    Speech { turn: TurnID },
}

/// Why the fold refused a record.
///
/// A call carries the command in two records, its own and the answer's, so the fold checks the two
/// agree: [`Refusal::CommandMismatch`] is that disagreement, and it names both commands because
/// [`Refusal::Conflict`] means one key recorded twice, not two records that must agree. A find
/// carries its query the same way, and [`Refusal::QueryMismatch`] is its twin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// A second record under one key carried different content.
    Conflict(RecordKey),
    /// A content record whose admitting machine fact the fold does not hold.
    Floating(RecordKey),
    /// A content record whose admitting fact the fold holds, arriving after a record it must precede.
    Late(RecordKey),
    /// A machine fact the fold refused.
    Rejected(Rejection),
    /// A call whose command the answer recorded for the same turn did not name.
    CommandMismatch {
        turn: TurnID,
        answer: String,
        call: String,
    },
    /// A find whose query the answer recorded for the same turn did not name.
    ///
    /// `answer` is the query that answer named, and empty when it named no query at all: an answer
    /// that asked for another tool asked for no search.
    QueryMismatch {
        turn: TurnID,
        answer: String,
        find: String,
    },
    /// An edit whose patch the answer recorded for the same turn did not name.
    ///
    /// `answer` is whatever that answer named instead: another patch, a command, or a query.
    EditMismatch {
        turn: TurnID,
        answer: String,
        edit: String,
    },
    /// An edit outcome whose intent the fold does not hold.
    Unprepared(ExecutionID),
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
            Self::Late(key) => {
                write!(formatter, "{key} arrived after a record it must precede")
            },
            Self::Rejected(rejection) => write!(formatter, "{rejection}"),
            Self::CommandMismatch { turn, answer, call } => write!(
                formatter,
                "the call for turn {} names {call:?} but its answer named {answer:?}",
                turn.as_uuid(),
            ),
            Self::QueryMismatch { turn, answer, find } => write!(
                formatter,
                "the find for turn {} asks {find:?} but its answer named {answer:?}",
                turn.as_uuid(),
            ),
            Self::EditMismatch { turn, answer, edit } => write!(
                formatter,
                "the edit for turn {} names {edit:?} but its answer named {answer:?}",
                turn.as_uuid(),
            ),
            Self::Unprepared(execution) => write!(
                formatter,
                "the edit for execution {} has no intent",
                execution.as_uuid(),
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
            Self::Find(turn) => write!(formatter, "the find for turn {}", turn.as_uuid()),
            Self::EditIntent(execution) => write!(
                formatter,
                "the edit intent for execution {}",
                execution.as_uuid(),
            ),
            Self::EditOutcome(execution) => write!(
                formatter,
                "the edit outcome for execution {}",
                execution.as_uuid(),
            ),
            Self::Speech { turn } => {
                write!(formatter, "the speech for turn {}", turn.as_uuid())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PATCH_BYTES, MAX_PROSE_BYTES, MAX_QUERY_BYTES, MAX_SPEECH_BYTES, clip_prose,
        clip_query, patch_refusal,
    };

    // The values are the durable log's byte bounds, and the kernel's provider clips prose to the
    // second one. Every other consumer reads them by name, so this suite is the only place a drift
    // in either number shows.
    #[test]
    fn the_log_bounds_hold_the_declared_byte_counts() {
        assert_eq!(MAX_SPEECH_BYTES, 4096);
        assert_eq!(MAX_PROSE_BYTES, 8192);
        assert_eq!(MAX_PATCH_BYTES, 32_768);
    }

    #[test]
    fn a_query_at_the_ceiling_is_kept_whole_and_a_longer_one_is_clipped_to_its_head() {
        let at_the_ceiling = "a".repeat(MAX_QUERY_BYTES);
        assert_eq!(clip_query(&at_the_ceiling), at_the_ceiling);

        let mut over_the_ceiling = "b".repeat(MAX_QUERY_BYTES);
        over_the_ceiling.push_str("clipped away");
        assert_eq!(clip_query(&over_the_ceiling), "b".repeat(MAX_QUERY_BYTES),);
    }

    #[test]
    fn a_query_past_its_ceiling_keeps_its_first_five_hundred_and_twelve_bytes() {
        assert_eq!(clip_query(&"a".repeat(600)), "a".repeat(512));
    }

    #[test]
    fn a_prose_past_its_ceiling_keeps_its_first_eight_thousand_one_hundred_and_ninety_two_bytes() {
        assert_eq!(clip_prose(&"a".repeat(9_000)), "a".repeat(8_192));
    }

    #[test]
    fn a_prose_at_its_ceiling_is_kept_whole() {
        let at_the_ceiling = "a".repeat(8_192);

        assert_eq!(clip_prose(&at_the_ceiling), at_the_ceiling);
    }

    #[test]
    fn a_patch_at_the_ceiling_is_allowed_and_one_byte_more_is_refused() {
        let at = "a".repeat(MAX_PATCH_BYTES);
        assert!(patch_refusal(&at).is_none());

        let over = "a".repeat(MAX_PATCH_BYTES + 1);
        assert_eq!(
            patch_refusal(&over),
            Some(format!(
                "the patch is {} bytes, and the log holds {}",
                over.len(),
                MAX_PATCH_BYTES,
            )),
        );
    }

    #[test]
    fn a_clipped_query_stops_on_a_character_boundary() {
        let mut text = "a".repeat(MAX_QUERY_BYTES - 1);
        text.push('é');
        text.push_str("and more");

        assert_eq!(clip_query(&text), "a".repeat(MAX_QUERY_BYTES - 1));
    }

    #[test]
    fn a_clipped_prose_stops_on_a_character_boundary() {
        let mut text = "a".repeat(8_191);
        text.push('é');
        text.push_str("and more");

        assert_eq!(clip_prose(&text), "a".repeat(8_191));
    }
}

//! Orchestrates model invocations, tools, sessions, and durable continuation.
//!
//! The crate is pure: it reads no clock, no file, and no socket, and no sandbox, store, or HTTP
//! type appears in it. Everything from the world enters through one of four ports — [`Journal`],
//! [`Provider`], [`Tools`], and [`Finder`] — and [`advance`] performs at most one bounded step.
//!
//! The loop is a total decision, [`next`], over the fold of a session's records, [`Transcript`].
//! Every effectful move spends two transactions: an intent is committed before the effect it
//! describes, and the outcome after it. Only the writer of an intent performs its effect, so a move
//! whose intent replayed yields rather than dispatching a second time. A model call is read-only, so
//! an interrupted one is re-asked under a new attempt with byte-identical context; a tool call may
//! have changed the world, so an interrupted one is never re-dispatched, and its unproven outcome
//! becomes [`UNKNOWN_WARNING`] in the next turn's prompt. The model, not the runner, reconciles.

mod advance;
mod context;
mod dialogue;
mod error;
mod find;
mod model;
mod record;
mod speech;
mod task;
mod tool;

#[cfg(test)]
mod test_support;

pub use advance::{Halt, Move, Progress, advance, execution_id, next, turn_id};
pub use context::{Context, ContextAct, ContextTurn, context, system_prompt};
pub use dialogue::Transcript;
pub use error::{Error, Source};
pub use find::{FindReport, Finder, NO_SEARCH_ROOT};
pub use model::{Action, Answer, Journal, Provider, Usage};
pub use record::{
    AnswerRecord, ArtifactRef, CallRecord, FindRecord, MAX_PROSE_BYTES, MAX_QUERY_BYTES,
    MAX_SPEECH_BYTES, PromptRecord, Receipt, Record, RecordKey, Refusal, SpeechRecord, Step,
    StepId, StepKey, TaskRecord, clip_prose, clip_query,
};
pub use speech::speak;
pub use task::{Budget, Setting, Task};
pub use tool::{
    DEFAULT_DEADLINE, Exit, ToolCall, ToolReport, Tools, UNKNOWN_WARNING, clip, observation,
};

//! Orchestrates model invocations, tools, sessions, and durable continuation.
//!
//! The crate is pure: it reads no clock, no file, and no socket, and no sandbox, store, or HTTP
//! type appears in it. Everything from the world enters through one of six ports — [`Journal`],
//! [`Provider`], [`Tools`], [`Finder`], [`Editor`], and [`Skills`] — and [`advance`] performs at
//! most one bounded step.
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
pub mod edit;
mod error;
mod find;
mod model;
mod record;
mod skill;
mod speech;
mod subagent;
mod task;
mod tool;

#[cfg(test)]
mod test_support;

pub use advance::{Halt, Move, Progress, advance, advance_watched, execution_id, next, turn_id};
pub use context::{Context, ContextAct, ContextSkill, ContextTurn, context, system_prompt};
pub use dialogue::Transcript;
pub use edit::{EditCall, EditOutcome, EditReport, Editor, Plan, Ready, unproven};
pub use error::{Error, Source};
pub use find::{FindReport, Finder, NO_SEARCH_ROOT};
pub use model::{Action, Answer, Journal, Provider, Usage};
pub use record::{
    AnswerRecord, ArtifactRef, CallRecord, EditIntentRecord, EditOutcomeRecord, FindRecord,
    MAX_PATCH_BYTES, MAX_PROSE_BYTES, MAX_QUERY_BYTES, MAX_SPEECH_BYTES, PromptRecord, Receipt,
    Record, RecordKey, Refusal, SkillOutcome, SkillRecord, SpeechRecord, Step, StepId, StepKey,
    SubagentIntentRecord, SubagentOutcomeRecord, TaskRecord, clip_prose, clip_query, patch_refusal,
};
pub use skill::{NO_SKILLS, Read, Skills};
pub use speech::{speak, speak_as};
pub use subagent::{
    SubagentCall, SubagentOutcome, Subagents, child_budget, child_session, child_task, clip_goal,
};
pub use task::{Advert, Budget, Setting, Task};
pub use tool::{
    DEFAULT_DEADLINE, Exit, ToolCall, ToolReport, Tools, UNKNOWN_WARNING, clip, observation,
};

/// The validated skill name a [`Action::UseSkill`] and a [`SkillRecord`] both carry.
pub use mokmok_skill::SkillName;

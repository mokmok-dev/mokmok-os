//! The control plane runtime: one actor per task, and the supervisor that runs them.
//!
//! [`ControlSystem`] keeps the event-sourced state of every task behind one actor per
//! task, so commands for the same task serialize while different tasks proceed
//! independently. [`Supervisor`] picks submitted tasks up, hands them to a [`Runner`],
//! and records each step: the domain state through the task's actor, and the transcript
//! through the [`Journal`].
//!
//! The domain and the event envelope come from [`mokmok_control_core`], and the
//! journal port and its event type are re-exported from [`mokmok_journal`] because
//! this crate's API exposes them. The rest of the journal crate is used directly.

mod runner;
mod supervisor;
mod system;

pub use mokmok_control_core::{
    AgentUpdate, CloudEvent, ControlCommand, ControlError, ControlEvent, DEFAULT_PROMPT_BYTES,
    EnumerableStore, EventStore, EventType, MAX_PROMPT_BYTES, MAX_UPDATES, PromptInput, SOURCE,
    SequenceType, SpecVersion, StoreError, StreamKey, TaskId, TaskSpec, TaskState, TaskStatus,
    Version, check_append, compose_prompt, decide, evolve, validate_history,
};
pub use mokmok_journal::{Journal, JournalEvent};
pub use runner::{
    AcpRunner, AgentConfig, AgentTable, AgentTransport, ConfigError, FailingRunner, ScriptedRunner,
};
pub use supervisor::{Reporter, RunOutcome, Runner, Supervisor};
pub use system::{
    Appended, ControlPublisher, ControlSystem, DEFAULT_IDLE_TIMEOUT, Error, NewTask, NoopPublisher,
    TaskSummary,
};

/// The journal shape of an update recorded before the execution journal existed.
///
/// The control plane's projection builds entries from this, so the wire shape has one
/// definition in the crate that knows both types.
#[must_use]
pub fn journal_event(update: &AgentUpdate) -> JournalEvent {
    match update {
        AgentUpdate::Message { text } => JournalEvent::Message { text: text.clone() },
        AgentUpdate::Thought { text } => JournalEvent::Thought { text: text.clone() },
        AgentUpdate::ToolCall { title, status } => JournalEvent::ToolCall {
            title: title.clone(),
            status: status.clone(),
        },
    }
}

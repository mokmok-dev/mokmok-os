//! The control plane runtime: one actor per task, and the supervisor that runs them.
//!
//! [`ControlSystem`] keeps the event-sourced state of every task behind one actor per
//! task, so commands for the same task serialize while different tasks proceed
//! independently. [`Supervisor`] picks submitted tasks up, hands them to a [`Runner`],
//! and records what the agent reports through the same actor, so every step is an
//! event in the task's stream.
//!
//! The domain and the event envelope come from [`mokmok_control_core`] and are
//! re-exported here so a caller needs one path.

mod runner;
mod supervisor;
mod system;

pub use mokmok_control_core::{
    AgentUpdate, CloudEvent, ControlCommand, ControlError, ControlEvent, DEFAULT_PROMPT_BYTES,
    EnumerableStore, EventStore, EventType, MAX_PROMPT_BYTES, MAX_UPDATES, PromptInput, SOURCE,
    SequenceType, SpecVersion, StoreError, StreamKey, TaskId, TaskSpec, TaskState, TaskStatus,
    Version, check_append, compose_prompt, decide, evolve, validate_history,
};
pub use runner::{
    AcpRunner, AgentConfig, AgentTable, AgentTransport, ConfigError, FailingRunner, ScriptedRunner,
};
pub use supervisor::{Reporter, RunOutcome, Runner, Supervisor};
pub use system::{
    Appended, ControlPublisher, ControlSystem, DEFAULT_IDLE_TIMEOUT, Error, NewTask, NoopPublisher,
    TaskSummary,
};

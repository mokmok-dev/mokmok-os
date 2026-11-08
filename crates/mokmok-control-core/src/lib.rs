//! Pure domain and transitions for the control plane.
//!
//! This crate holds no I/O, no threads, and no clock: it decides, evolves, and
//! replays. Every state change is a [`CloudEvent`] in a task's stream, and the
//! envelope and store ports come from [`mokmok_events`] and are re-exported here so a
//! caller needs one path.
//!
//! [`prompt`] composes the agent prompt from standard-library pieces alone, so the
//! proof-only crate can include that same source file.

pub mod prompt;
mod task;

pub use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, SOURCE, SequenceType, SpecVersion,
    StoreError, StreamKey, Version, check_append, validate_history,
};
pub use prompt::{
    DEFAULT_PROMPT_BYTES, MAX_PROMPT_BYTES, PromptInput, clip_to_char_boundary, compose_prompt,
};
pub use task::{
    AgentUpdate, ControlCommand, ControlError, ControlEvent, MAX_UPDATES, TaskId, TaskSpec,
    TaskState, TaskStatus, decide, evolve,
};

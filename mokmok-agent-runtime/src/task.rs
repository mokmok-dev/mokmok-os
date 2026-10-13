//! What the operator asked a session to do, and the limits it runs under.

use std::path::PathBuf;
use std::time::Duration;

/// The goal, the world it runs in, and the budget it may spend.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Task {
    pub goal: String,
    pub setting: Setting,
    pub budget: Budget,
}

/// What the model is told it has, and nothing about how the operator enforces it.
///
/// The grant that confines a command is the operator's; nothing here is a policy or a capability,
/// so a `Policy`, `Rule`, or grant never reaches the runtime.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Setting {
    /// The scratch checkout the goal lives in, reached by absolute path inside a command.
    pub checkout: PathBuf,
    /// Directories the operator granted for reading and executing.
    pub readable: Vec<PathBuf>,
    /// Directories the operator granted for reading, writing, and executing.
    pub writable: Vec<PathBuf>,
    /// The one program a command runs as, named by the operator and never by the model.
    pub program: PathBuf,
    /// How long one command may run, as the operator set it when the task was recorded.
    pub deadline: Duration,
}

/// The ceilings one session runs under.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Budget {
    /// How many turns the session may open.
    pub turns: u32,
    /// How many model attempts one turn may spend before it closes failed.
    pub attempts: u32,
    /// How many tokens the session may spend; zero means unbounded.
    pub tokens: u64,
    /// How many bytes of one observation the model is shown.
    pub observation_bytes: u64,
    /// How many settled turns the model is shown.
    pub context_turns: u32,
    /// How many bytes of context the model is shown in total.
    pub context_bytes: u64,
}

impl Budget {
    /// The attempt ceiling a turn gets when the caller names none.
    pub const DEFAULT_ATTEMPTS: u32 = 3;
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            turns: 0,
            attempts: Self::DEFAULT_ATTEMPTS,
            tokens: 0,
            observation_bytes: 0,
            context_turns: 0,
            context_bytes: 0,
        }
    }
}

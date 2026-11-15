//! What a page needs from the control plane.
//!
//! The pages call this port instead of the daemon's `AppState`, so the crate renders
//! against a small set of reads and the daemon owns the wiring. The reads are async
//! because the same port serves a local file store and an object store whose reads are
//! network round trips.

use std::future::Future;
use std::pin::Pin;

use mokmok_control_core::{TaskId, TaskStatus};
use mokmok_journal::JournalEvent;

/// A future a port method returns, boxed so the trait stays usable behind `dyn`.
pub type Read<'a, T> = Pin<Box<dyn Future<Output = Result<T, PortError>> + Send + 'a>>;

/// One task as a listing row sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: TaskId,
    pub title: String,
    /// The instruction cut to the listing budget, with an ellipsis when it was cut.
    pub instruction: String,
    pub instruction_truncated: bool,
    pub status: TaskStatus,
    pub agent: String,
    pub workspace: Option<String>,
    pub outcome: Option<String>,
    /// Journal entry count, when the daemon could read it.
    pub updates: Option<u64>,
}

/// A transcript entry with the sequence number the journal stamped on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub event: JournalEvent,
}

/// One session's transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// Entries the journal holds, which can exceed the ones returned.
    pub total: u64,
}

/// Why a page could not read or change the control plane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortError {
    /// The task id is not a UUID, or the task does not exist.
    NotFound,
    /// The task already finished, so the command was refused.
    Finished,
    /// The daemon refused the command for another reason.
    Refused(String),
    /// The read or write failed.
    Unavailable,
}

impl std::fmt::Display for PortError {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("no such task"),
            Self::Finished => formatter.write_str("the task has already finished"),
            Self::Refused(reason) => formatter.write_str(reason),
            Self::Unavailable => formatter.write_str("the control plane is unavailable"),
        }
    }
}

impl std::error::Error for PortError {}

/// The reads and commands the pages need.
pub trait ControlPlane: Send + Sync {
    /// Every task, as the listing sees it.
    fn sessions(&self) -> Read<'_, Vec<Session>>;

    /// The status and outcome of one task.
    fn session_state(
        &self,
        id: TaskId,
    ) -> Read<'_, SessionState>;

    /// The newest entries of one task's transcript.
    fn transcript(
        &self,
        id: TaskId,
        limit: usize,
    ) -> Read<'_, Transcript>;

    /// The agent names a new task may choose, and the default.
    fn agents(&self) -> Agents;

    /// Submits a task and returns its id.
    fn submit(
        &self,
        spec: NewSession,
    ) -> Read<'_, TaskId>;

    /// Asks the running task to stop.
    fn cancel(
        &self,
        id: TaskId,
        reason: String,
    ) -> Read<'_, ()>;
}

/// The status and outcome of one task, which is all a live page re-reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub status: TaskStatus,
    pub outcome: Option<String>,
}

/// The agent choices a new session may make.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agents {
    pub names: Vec<String>,
    pub default: String,
}

/// The instruction a form submits, before the daemon mints an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSession {
    pub title: String,
    pub instruction: String,
    pub agent: String,
    pub workspace: Option<String>,
}

impl NewSession {
    /// Builds a session from form fields, deriving the title from the instruction.
    #[must_use]
    pub fn from_form(
        instruction: &str,
        agent: &str,
        workspace: Option<&str>,
    ) -> Self {
        Self {
            title: title_from_instruction(instruction),
            instruction: instruction.to_owned(),
            agent: agent.to_owned(),
            workspace: workspace
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        }
    }
}

/// The largest title the domain accepts.
const MAX_TITLE_BYTES: usize = 200;

/// The title a blank instruction gets.
pub const FALLBACK_TITLE: &str = "Untitled session";

/// Derives a title from an instruction the way the session list shows it.
///
/// The first line wins, every carriage return is dropped, and the result is cut to the
/// domain's 200-byte title budget on a character boundary. Carriage returns go before
/// the cut because the domain refuses a title that holds one. A blank instruction has no
/// title to derive, so it becomes [`FALLBACK_TITLE`].
#[must_use]
pub fn title_from_instruction(instruction: &str) -> String {
    let first = instruction.lines().next().unwrap_or_default();
    let line = first.replace('\r', "");
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return FALLBACK_TITLE.to_owned();
    }
    if trimmed.len() <= MAX_TITLE_BYTES {
        return trimmed.to_owned();
    }
    trimmed[..trimmed.floor_char_boundary(MAX_TITLE_BYTES)].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_the_first_trimmed_line() {
        assert_eq!(title_from_instruction("  first line\nsecond"), "first line");
        assert_eq!(title_from_instruction("only"), "only");
    }

    #[test]
    fn a_blank_instruction_gets_the_fallback_title() {
        assert_eq!(title_from_instruction("   \n "), FALLBACK_TITLE);
        assert_eq!(title_from_instruction(""), FALLBACK_TITLE);
    }

    #[test]
    fn a_carriage_return_never_reaches_the_title() {
        assert_eq!(title_from_instruction("one\r"), "one");
        assert_eq!(title_from_instruction("a\rb"), "ab");
    }

    #[test]
    fn a_long_title_is_cut_to_the_domain_budget_on_a_character_boundary() {
        let title = title_from_instruction(&"é".repeat(MAX_TITLE_BYTES));
        assert_eq!(title, "é".repeat(MAX_TITLE_BYTES / 2));
        assert_eq!(title.len(), MAX_TITLE_BYTES);
    }

    #[test]
    fn a_title_at_the_budget_keeps_every_character() {
        let instruction = "a".repeat(MAX_TITLE_BYTES);
        assert_eq!(title_from_instruction(&instruction), instruction);
    }

    #[test]
    fn a_blank_workspace_field_means_no_workspace() {
        let session = NewSession::from_form("do it", "local", Some("  "));
        assert_eq!(session.workspace, None);
        let session = NewSession::from_form("do it", "local", Some(" /w "));
        assert_eq!(session.workspace.as_deref(), Some("/w"));
    }
}

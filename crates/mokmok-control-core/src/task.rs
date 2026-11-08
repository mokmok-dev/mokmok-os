//! The task state machine: identity, specs, events, commands, and the decisions
//! between them.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mokmok_events::{CloudEvent, EventType, StoreError, StreamKey, validate_history};

/// How many of a task's most recent updates its state keeps.
pub const MAX_UPDATES: usize = 4096;

const MAX_TITLE_BYTES: usize = 200;
const MAX_INSTRUCTION_BYTES: usize = 65_536;
const MAX_WORKSPACE_BYTES: usize = 4096;
const MAX_AGENT_BYTES: usize = 32;
const MAX_HOST_BYTES: usize = 64;

/// The stable identity of one control-plane task.
///
/// The text is the canonical lowercase hyphenated UUID, so two spellings of the same
/// id never name two streams. [`from_u128`](Self::from_u128) builds an id from a fixed
/// number for tests and externally allocated ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TaskId(String);

impl TaskId {
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTaskId`] when the raw value is not a UUID.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ControlError> {
        let raw = raw.into();
        Uuid::parse_str(&raw).map_or_else(
            |_| Err(ControlError::InvalidTaskId(raw)),
            |uuid| Ok(Self::from_uuid(uuid)),
        )
    }

    #[must_use]
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid.hyphenated().to_string())
    }

    #[must_use]
    pub fn from_u128(value: u128) -> Self {
        Self::from_uuid(Uuid::from_u128(value))
    }

    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TryFrom<String> for TaskId {
    type Error = ControlError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(raw)
    }
}

impl From<TaskId> for String {
    fn from(id: TaskId) -> Self {
        id.0
    }
}

impl StreamKey for TaskId {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }

    fn parse_key(raw: &str) -> Result<Self, StoreError> {
        Self::parse(raw).map_err(|error| StoreError::Invalid(error.to_string()))
    }
}

/// Everything a submission carries, validated before it becomes an event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task_id: TaskId,
    pub title: String,
    pub instruction: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl TaskSpec {
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidSpec`] naming the first field that fails.
    pub fn validate(&self) -> Result<(), ControlError> {
        validate_title(&self.title)?;
        validate_text("instruction", &self.instruction, MAX_INSTRUCTION_BYTES)?;
        validate_agent(&self.agent)?;
        if let Some(host) = &self.host {
            validate_host(host)?;
        }
        if let Some(workspace) = &self.workspace {
            validate_workspace(workspace)?;
        }
        Ok(())
    }
}

fn validate_title(title: &str) -> Result<(), ControlError> {
    validate_text("title", title, MAX_TITLE_BYTES)?;
    if title.contains('\n') || title.contains('\r') {
        return Err(ControlError::InvalidSpec {
            field: "title",
            reason: "must not contain a line break".to_owned(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    text: &str,
    max_bytes: usize,
) -> Result<(), ControlError> {
    let reason = if text.is_empty() {
        "must not be empty".to_owned()
    } else if text.len() > max_bytes {
        format!("must be at most {max_bytes} bytes")
    } else if text.contains('\0') {
        "must not contain NUL".to_owned()
    } else {
        return Ok(());
    };
    Err(ControlError::InvalidSpec { field, reason })
}

const fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn validate_agent(agent: &str) -> Result<(), ControlError> {
    let bytes = agent.as_bytes();
    let valid = bytes.len() <= MAX_AGENT_BYTES
        && bytes.first().is_some_and(|byte| is_lower_alnum(*byte))
        && bytes
            .iter()
            .all(|byte| is_lower_alnum(*byte) || *byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ControlError::InvalidSpec {
            field: "agent",
            reason: "must match [a-z0-9][a-z0-9-]{0,31}".to_owned(),
        })
    }
}

fn validate_host(host: &str) -> Result<(), ControlError> {
    let bytes = host.as_bytes();
    let valid = bytes.len() <= MAX_HOST_BYTES
        && bytes.first().is_some_and(|byte| is_lower_alnum(*byte))
        && bytes
            .iter()
            .all(|byte| is_lower_alnum(*byte) || *byte == b'.' || *byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ControlError::InvalidSpec {
            field: "host",
            reason: "must match [a-z0-9][a-z0-9.-]{0,63}".to_owned(),
        })
    }
}

fn validate_workspace(workspace: &str) -> Result<(), ControlError> {
    let reason = if !workspace.starts_with('/') {
        "must be an absolute path".to_owned()
    } else if workspace.len() > MAX_WORKSPACE_BYTES {
        format!("must be at most {MAX_WORKSPACE_BYTES} bytes")
    } else if workspace.contains('\0') {
        "must not contain NUL".to_owned()
    } else if workspace.contains('\n') || workspace.contains('\r') {
        "must not contain a line break".to_owned()
    } else {
        return Ok(());
    };
    Err(ControlError::InvalidSpec {
        field: "workspace",
        reason,
    })
}

/// One step an agent reported while running a task.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentUpdate {
    Message { text: String },
    Thought { text: String },
    ToolCall { title: String, status: String },
}

/// A request to change a task, before it becomes an event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlCommand {
    Submit(TaskSpec),
    Start,
    OpenSession { session_id: String },
    Complete { summary: String },
    Fail { reason: String },
    Cancel { reason: String },
}

/// One committed fact about a task.
///
/// [`UpdateRecorded`](Self::UpdateRecorded) survives only so a task logged before the
/// execution journal existed still replays; no command produces one now.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlEvent {
    Submitted { spec: TaskSpec },
    Started,
    SessionOpened { session_id: String },
    UpdateRecorded { update: AgentUpdate },
    Completed { summary: String },
    Failed { reason: String },
    Cancelled { reason: String },
}

impl ControlEvent {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "submitted",
            Self::Started => "started",
            Self::SessionOpened { .. } => "session_opened",
            Self::UpdateRecorded { .. } => "update_recorded",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

impl EventType for ControlEvent {
    fn event_type(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "dev.mokmok.control.submitted",
            Self::Started => "dev.mokmok.control.started",
            Self::SessionOpened { .. } => "dev.mokmok.control.session_opened",
            Self::UpdateRecorded { .. } => "dev.mokmok.control.update_recorded",
            Self::Completed { .. } => "dev.mokmok.control.completed",
            Self::Failed { .. } => "dev.mokmok.control.failed",
            Self::Cancelled { .. } => "dev.mokmok.control.cancelled",
        }
    }
}

/// Where one task stands, derived only from its event stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    New,
    Submitted,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    const fn kind(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Submitted => "submitted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Everything a task's event stream says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<TaskSpec>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<AgentUpdate>,
    pub update_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Why a task id, spec, command, or recorded event was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    InvalidTaskId(String),
    InvalidSpec { field: &'static str, reason: String },
    WrongTaskId { stream: String, spec: String },
    NotSubmitted,
    AlreadySubmitted,
    AlreadyStarted,
    NotRunning,
    SessionAlreadyOpen,
    Terminal,
    CorruptHistory(String),
}

impl fmt::Display for ControlError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::InvalidTaskId(raw) => write!(formatter, "invalid task id {raw:?}"),
            Self::InvalidSpec { field, reason } => {
                write!(formatter, "task spec {field} {reason}")
            },
            Self::WrongTaskId { stream, spec } => write!(
                formatter,
                "task spec names {spec}, but its stream is {stream}"
            ),
            Self::NotSubmitted => formatter.write_str("task was not submitted"),
            Self::AlreadySubmitted => formatter.write_str("task was already submitted"),
            Self::AlreadyStarted => formatter.write_str("task was already started"),
            Self::NotRunning => formatter.write_str("task is not running"),
            Self::SessionAlreadyOpen => formatter.write_str("task already has an open session"),
            Self::Terminal => formatter.write_str("task is in a terminal state"),
            Self::CorruptHistory(message) => write!(formatter, "corrupt history: {message}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// Decides the events a command commits, or why it is rejected.
///
/// # Errors
///
/// Returns [`ControlError::InvalidSpec`] for a submission that fails validation, and
/// the variant naming the state the command does not fit.
pub fn decide(
    state: &TaskState,
    command: ControlCommand,
) -> Result<Vec<ControlEvent>, ControlError> {
    match state.status {
        TaskStatus::New => match command {
            ControlCommand::Submit(spec) => {
                spec.validate()?;
                Ok(vec![ControlEvent::Submitted { spec }])
            },
            ControlCommand::Start
            | ControlCommand::OpenSession { .. }
            | ControlCommand::Complete { .. }
            | ControlCommand::Fail { .. }
            | ControlCommand::Cancel { .. } => Err(ControlError::NotSubmitted),
        },
        TaskStatus::Submitted => match command {
            ControlCommand::Start => Ok(vec![ControlEvent::Started]),
            ControlCommand::Cancel { reason } => Ok(vec![ControlEvent::Cancelled { reason }]),
            ControlCommand::Submit(_) => Err(ControlError::AlreadySubmitted),
            ControlCommand::OpenSession { .. }
            | ControlCommand::Complete { .. }
            | ControlCommand::Fail { .. } => Err(ControlError::NotRunning),
        },
        TaskStatus::Running => match command {
            ControlCommand::OpenSession { session_id } => {
                if state.session.is_some() {
                    Err(ControlError::SessionAlreadyOpen)
                } else {
                    Ok(vec![ControlEvent::SessionOpened { session_id }])
                }
            },
            ControlCommand::Complete { summary } => Ok(vec![ControlEvent::Completed { summary }]),
            ControlCommand::Fail { reason } => Ok(vec![ControlEvent::Failed { reason }]),
            ControlCommand::Cancel { reason } => Ok(vec![ControlEvent::Cancelled { reason }]),
            ControlCommand::Start => Err(ControlError::AlreadyStarted),
            ControlCommand::Submit(_) => Err(ControlError::AlreadySubmitted),
        },
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
            Err(ControlError::Terminal)
        },
    }
}

/// Applies one committed event to a state, rejecting a pair no decision produces.
///
/// # Errors
///
/// Returns [`ControlError::CorruptHistory`] when the event does not fit the state.
pub fn evolve(
    state: &mut TaskState,
    event: &ControlEvent,
) -> Result<(), ControlError> {
    match (state.status, event) {
        (TaskStatus::New, ControlEvent::Submitted { spec }) => {
            state.spec = Some(spec.clone());
            state.status = TaskStatus::Submitted;
        },
        (TaskStatus::Submitted, ControlEvent::Started) => {
            state.status = TaskStatus::Running;
        },
        (TaskStatus::Running, ControlEvent::SessionOpened { session_id })
            if state.session.is_none() =>
        {
            state.session = Some(session_id.clone());
        },
        (TaskStatus::Running, ControlEvent::UpdateRecorded { update }) => {
            if state.updates.len() == MAX_UPDATES {
                state.updates.remove(0);
            }
            state.updates.push(update.clone());
            state.update_count = state.update_count.saturating_add(1);
        },
        (TaskStatus::Running, ControlEvent::Completed { summary }) => {
            state.status = TaskStatus::Completed;
            state.outcome = Some(summary.clone());
        },
        (TaskStatus::Running, ControlEvent::Failed { reason }) => {
            state.status = TaskStatus::Failed;
            state.outcome = Some(reason.clone());
        },
        (TaskStatus::Submitted | TaskStatus::Running, ControlEvent::Cancelled { reason }) => {
            state.status = TaskStatus::Cancelled;
            state.outcome = Some(reason.clone());
        },
        _ => {
            return Err(ControlError::CorruptHistory(format!(
                "state {} cannot take event {}",
                state.status.kind(),
                event.kind()
            )));
        },
    }
    Ok(())
}

impl TaskState {
    /// Rebuilds the state of one stream, enforcing one subject and consecutive versions.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::CorruptHistory`] when the records mix subjects, skip a
    /// version, or carry an event the preceding state cannot take.
    pub fn replay(events: &[CloudEvent<ControlEvent>]) -> Result<Self, ControlError> {
        let Some(first) = events.first() else {
            return Ok(Self::default());
        };
        validate_history(&first.subject, events)
            .map_err(|error| ControlError::CorruptHistory(error.to_string()))?;
        let mut state = Self::default();
        for event in events {
            evolve(&mut state, &event.data)?;
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use mokmok_events::Version;

    use super::*;

    fn spec() -> TaskSpec {
        TaskSpec {
            task_id: TaskId::from_u128(1),
            title: "Fix the build".to_owned(),
            instruction: "Make the tests pass.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: Some("/work/mokmok".to_owned()),
        }
    }

    fn submitted() -> ControlEvent {
        ControlEvent::Submitted { spec: spec() }
    }

    fn message() -> AgentUpdate {
        AgentUpdate::Message {
            text: "looking".to_owned(),
        }
    }

    fn reached(status: TaskStatus) -> TaskState {
        let chain: Vec<ControlEvent> = match status {
            TaskStatus::New => Vec::new(),
            TaskStatus::Submitted => vec![submitted()],
            TaskStatus::Running => vec![submitted(), ControlEvent::Started],
            TaskStatus::Completed => vec![
                submitted(),
                ControlEvent::Started,
                ControlEvent::Completed {
                    summary: "done".to_owned(),
                },
            ],
            TaskStatus::Failed => vec![
                submitted(),
                ControlEvent::Started,
                ControlEvent::Failed {
                    reason: "broken".to_owned(),
                },
            ],
            TaskStatus::Cancelled => vec![
                submitted(),
                ControlEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
            ],
        };
        let mut state = TaskState::default();
        for event in chain {
            evolve(&mut state, &event).expect("the chain is legal");
        }
        state
    }

    fn session_open() -> TaskState {
        let mut state = reached(TaskStatus::Running);
        evolve(
            &mut state,
            &ControlEvent::SessionOpened {
                session_id: "session-1".to_owned(),
            },
        )
        .expect("the first session opens");
        state
    }

    fn every_command() -> Vec<ControlCommand> {
        vec![
            ControlCommand::Submit(spec()),
            ControlCommand::Start,
            ControlCommand::OpenSession {
                session_id: "session-1".to_owned(),
            },
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
            ControlCommand::Fail {
                reason: "broken".to_owned(),
            },
            ControlCommand::Cancel {
                reason: "stop".to_owned(),
            },
        ]
    }

    #[test]
    fn task_id_parses_a_uuid() {
        let raw = "01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60";
        let id = TaskId::parse(raw).expect("literal is a uuid");
        assert_eq!(id.as_str(), raw);
    }

    #[test]
    fn task_id_canonicalizes_every_uuid_spelling() {
        let canonical = "01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60";
        for raw in [
            canonical.to_uppercase(),
            "01890b0c7f3a7c2e9f4a2b1c3d4e5f60".to_owned(),
            format!("urn:uuid:{canonical}"),
            format!("{{{canonical}}}"),
        ] {
            let id = TaskId::parse(raw).expect("the form is a uuid");
            assert_eq!(id.as_str(), canonical);
        }
    }

    #[test]
    fn task_id_rejects_non_uuid_input() {
        for raw in ["bad id".to_owned(), ":".repeat(128), String::new()] {
            assert!(matches!(
                TaskId::parse(raw),
                Err(ControlError::InvalidTaskId(_))
            ));
        }
    }

    #[test]
    fn from_u128_is_a_fixed_id() {
        assert_eq!(
            TaskId::from_u128(1).as_str(),
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn the_stream_key_names_the_canonical_uuid() {
        assert_eq!(
            StreamKey::as_str(&TaskId::from_u128(0x1234)),
            "00000000-0000-0000-0000-000000001234"
        );
        assert_eq!(
            StreamKey::as_str(&TaskId::from_u128(1)),
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn task_id_string_conversion_validates() {
        let id = TaskId::try_from("01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60".to_owned())
            .expect("literal is a uuid");
        assert_eq!(String::from(id), "01890b0c-7f3a-7c2e-9f4a-2b1c3d4e5f60");
        assert!(TaskId::try_from(String::new()).is_err());
    }

    #[test]
    fn parse_key_round_trips_a_task_id() {
        let id = TaskId::from_u128(9);
        assert_eq!(TaskId::parse_key(id.as_str()), Ok(id));
    }

    #[test]
    fn parse_key_refuses_a_name_that_is_not_a_uuid() {
        assert!(matches!(
            TaskId::parse_key("task-1"),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn validate_accepts_a_minimal_and_a_full_spec() {
        assert_eq!(spec().validate(), Ok(()));
        let mut minimal = spec();
        minimal.host = None;
        minimal.workspace = None;
        assert_eq!(minimal.validate(), Ok(()));
        minimal.title = "t".to_owned();
        minimal.instruction = "i".to_owned();
        minimal.agent = "a".to_owned();
        assert_eq!(minimal.validate(), Ok(()));
    }

    #[test]
    fn validate_accepts_the_boundary_lengths() {
        let mut boundary = spec();
        boundary.title = "t".repeat(MAX_TITLE_BYTES);
        boundary.instruction = "i".repeat(MAX_INSTRUCTION_BYTES);
        boundary.agent = format!("a{}", "-".repeat(MAX_AGENT_BYTES - 1));
        boundary.host = Some(format!("h{}", "-".repeat(MAX_HOST_BYTES - 1)));
        boundary.workspace = Some(format!("/{}", "w".repeat(MAX_WORKSPACE_BYTES - 1)));
        assert_eq!(boundary.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_a_bad_title() {
        for (title, reason) in [
            (String::new(), "must not be empty"),
            ("t".repeat(MAX_TITLE_BYTES + 1), "must be at most 200 bytes"),
            ("ti\0tle".to_owned(), "must not contain NUL"),
            ("line\nbreak".to_owned(), "must not contain a line break"),
            (
                "carriage\rreturn".to_owned(),
                "must not contain a line break",
            ),
        ] {
            let mut invalid = spec();
            invalid.title = title;
            let error = invalid.validate().expect_err("the title is invalid");
            assert!(error.to_string().contains("title"), "{error}");
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[test]
    fn validate_rejects_a_bad_instruction() {
        for (instruction, reason) in [
            (String::new(), "must not be empty"),
            (
                "i".repeat(MAX_INSTRUCTION_BYTES + 1),
                "must be at most 65536 bytes",
            ),
            ("in\0struction".to_owned(), "must not contain NUL"),
        ] {
            let mut invalid = spec();
            invalid.instruction = instruction;
            let error = invalid.validate().expect_err("the instruction is invalid");
            assert!(error.to_string().contains("instruction"), "{error}");
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[test]
    fn validate_rejects_a_bad_agent() {
        for agent in [
            String::new(),
            "Scripted".to_owned(),
            "scripted_agent".to_owned(),
            "-scripted".to_owned(),
            "a".repeat(MAX_AGENT_BYTES + 1),
            "agént".to_owned(),
        ] {
            let mut invalid = spec();
            invalid.agent = agent.clone();
            let error = invalid.validate().expect_err("the agent is invalid");
            assert!(error.to_string().contains("agent"), "{error}");
            assert!(error.to_string().contains("[a-z0-9]"), "{error}");
        }
    }

    #[test]
    fn validate_rejects_a_bad_host() {
        for host in [
            String::new(),
            "Remote.Host".to_owned(),
            ".remote".to_owned(),
            "remote/path".to_owned(),
            "h".repeat(MAX_HOST_BYTES + 1),
        ] {
            let mut invalid = spec();
            invalid.host = Some(host);
            let error = invalid.validate().expect_err("the host is invalid");
            assert!(error.to_string().contains("host"), "{error}");
        }
    }

    #[test]
    fn validate_rejects_a_bad_workspace() {
        for (workspace, reason) in [
            (String::new(), "must be an absolute path"),
            ("relative/path".to_owned(), "must be an absolute path"),
            ("/work\0".to_owned(), "must not contain NUL"),
            (
                format!("/{}", "w".repeat(MAX_WORKSPACE_BYTES)),
                "must be at most 4096 bytes",
            ),
            ("/work\npath".to_owned(), "must not contain a line break"),
            ("/work\rpath".to_owned(), "must not contain a line break"),
        ] {
            let mut invalid = spec();
            invalid.workspace = Some(workspace);
            let error = invalid.validate().expect_err("the workspace is invalid");
            assert!(error.to_string().contains("workspace"), "{error}");
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[test]
    fn decide_accepts_submit_only_on_new() {
        assert_eq!(
            decide(&TaskState::default(), ControlCommand::Submit(spec())),
            Ok(vec![submitted()])
        );
        for command in every_command().into_iter().skip(1) {
            assert_eq!(
                decide(&TaskState::default(), command),
                Err(ControlError::NotSubmitted)
            );
        }
    }

    #[test]
    fn decide_rejects_an_invalid_submission() {
        let mut invalid = spec();
        invalid.agent = "Scripted".to_owned();
        assert!(matches!(
            decide(&TaskState::default(), ControlCommand::Submit(invalid)),
            Err(ControlError::InvalidSpec { field: "agent", .. })
        ));
    }

    #[test]
    fn decide_in_the_submitted_state() {
        let state = reached(TaskStatus::Submitted);
        assert_eq!(
            decide(&state, ControlCommand::Start),
            Ok(vec![ControlEvent::Started])
        );
        assert_eq!(
            decide(
                &state,
                ControlCommand::Cancel {
                    reason: "stop".to_owned(),
                }
            ),
            Ok(vec![ControlEvent::Cancelled {
                reason: "stop".to_owned(),
            }])
        );
        assert_eq!(
            decide(&state, ControlCommand::Submit(spec())),
            Err(ControlError::AlreadySubmitted)
        );
        for command in [
            ControlCommand::OpenSession {
                session_id: "session-1".to_owned(),
            },
            ControlCommand::Complete {
                summary: "done".to_owned(),
            },
            ControlCommand::Fail {
                reason: "broken".to_owned(),
            },
        ] {
            assert_eq!(decide(&state, command), Err(ControlError::NotRunning));
        }
    }

    #[test]
    fn decide_in_the_running_state() {
        let state = reached(TaskStatus::Running);
        assert_eq!(
            decide(
                &state,
                ControlCommand::OpenSession {
                    session_id: "session-1".to_owned(),
                }
            ),
            Ok(vec![ControlEvent::SessionOpened {
                session_id: "session-1".to_owned(),
            }])
        );
        assert_eq!(
            decide(
                &state,
                ControlCommand::Complete {
                    summary: "done".to_owned(),
                }
            ),
            Ok(vec![ControlEvent::Completed {
                summary: "done".to_owned(),
            }])
        );
        assert_eq!(
            decide(
                &state,
                ControlCommand::Fail {
                    reason: "broken".to_owned(),
                }
            ),
            Ok(vec![ControlEvent::Failed {
                reason: "broken".to_owned(),
            }])
        );
        assert_eq!(
            decide(
                &state,
                ControlCommand::Cancel {
                    reason: "stop".to_owned(),
                }
            ),
            Ok(vec![ControlEvent::Cancelled {
                reason: "stop".to_owned(),
            }])
        );
        assert_eq!(
            decide(&state, ControlCommand::Start),
            Err(ControlError::AlreadyStarted)
        );
        assert_eq!(
            decide(&state, ControlCommand::Submit(spec())),
            Err(ControlError::AlreadySubmitted)
        );
    }

    #[test]
    fn decide_rejects_a_second_session() {
        let state = session_open();
        assert_eq!(
            decide(
                &state,
                ControlCommand::OpenSession {
                    session_id: "session-2".to_owned(),
                }
            ),
            Err(ControlError::SessionAlreadyOpen)
        );
    }

    #[test]
    fn decide_rejects_every_command_in_a_terminal_state() {
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            let state = reached(status);
            assert_eq!(state.status, status);
            for command in every_command() {
                assert_eq!(decide(&state, command), Err(ControlError::Terminal));
            }
        }
    }

    #[test]
    fn evolve_applies_every_legal_event() {
        let mut state = TaskState::default();
        evolve(&mut state, &submitted()).expect("submitted follows new");
        assert_eq!(state.spec, Some(spec()));
        assert_eq!(state.status, TaskStatus::Submitted);

        evolve(&mut state, &ControlEvent::Started).expect("started follows submitted");
        assert_eq!(state.status, TaskStatus::Running);

        evolve(
            &mut state,
            &ControlEvent::SessionOpened {
                session_id: "session-1".to_owned(),
            },
        )
        .expect("the first session opens");
        assert_eq!(state.session.as_deref(), Some("session-1"));

        evolve(
            &mut state,
            &ControlEvent::UpdateRecorded {
                update: AgentUpdate::Thought {
                    text: "thinking".to_owned(),
                },
            },
        )
        .expect("an update follows running");
        assert_eq!(state.updates.len(), 1);
        assert_eq!(state.update_count, 1);

        evolve(
            &mut state,
            &ControlEvent::Completed {
                summary: "done".to_owned(),
            },
        )
        .expect("completed follows running");
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.outcome.as_deref(), Some("done"));
        assert_eq!(state.session.as_deref(), Some("session-1"));

        let mut failed = reached(TaskStatus::Running);
        evolve(
            &mut failed,
            &ControlEvent::Failed {
                reason: "broken".to_owned(),
            },
        )
        .expect("failed follows running");
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(failed.outcome.as_deref(), Some("broken"));

        let mut cancelled = reached(TaskStatus::Submitted);
        evolve(
            &mut cancelled,
            &ControlEvent::Cancelled {
                reason: "stop".to_owned(),
            },
        )
        .expect("cancelled follows submitted");
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(cancelled.outcome.as_deref(), Some("stop"));
    }

    #[test]
    fn evolve_rejects_every_illegal_pair() {
        let pairs = [
            (TaskState::default(), ControlEvent::Started),
            (
                TaskState::default(),
                ControlEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
            ),
            (
                reached(TaskStatus::Submitted),
                ControlEvent::UpdateRecorded { update: message() },
            ),
            (
                reached(TaskStatus::Submitted),
                ControlEvent::Completed {
                    summary: "done".to_owned(),
                },
            ),
            (reached(TaskStatus::Running), ControlEvent::Started),
            (reached(TaskStatus::Running), submitted()),
            (
                session_open(),
                ControlEvent::SessionOpened {
                    session_id: "session-2".to_owned(),
                },
            ),
            (
                reached(TaskStatus::Completed),
                ControlEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
            ),
            (
                reached(TaskStatus::Failed),
                ControlEvent::UpdateRecorded { update: message() },
            ),
        ];
        for (mut state, event) in pairs {
            let before = state.clone();
            assert!(matches!(
                evolve(&mut state, &event),
                Err(ControlError::CorruptHistory(_))
            ));
            assert_eq!(state, before);
        }
    }

    #[test]
    fn the_update_cap_is_four_thousand_ninety_six() {
        assert_eq!(MAX_UPDATES, 4096);
    }

    #[test]
    fn evolve_caps_the_updates_and_counts_every_one() {
        let mut state = reached(TaskStatus::Running);
        let total = MAX_UPDATES + 5;
        for index in 0..total {
            evolve(
                &mut state,
                &ControlEvent::UpdateRecorded {
                    update: AgentUpdate::Message {
                        text: format!("u{index}"),
                    },
                },
            )
            .expect("an update follows running");
        }
        assert_eq!(state.updates.len(), MAX_UPDATES);
        assert_eq!(state.update_count, total as u64);
        assert_eq!(
            state.updates.first(),
            Some(&AgentUpdate::Message {
                text: "u5".to_owned()
            })
        );
        assert_eq!(
            state.updates.last(),
            Some(&AgentUpdate::Message {
                text: format!("u{}", total - 1),
            })
        );
    }

    #[test]
    fn replay_rebuilds_a_legacy_log_with_updates() {
        let id = TaskId::from_u128(3);
        let total = MAX_UPDATES + 5;
        let mut events = vec![
            CloudEvent::new(id.as_str(), Version::new(1), submitted()),
            CloudEvent::new(id.as_str(), Version::new(2), ControlEvent::Started),
            CloudEvent::new(
                id.as_str(),
                Version::new(3),
                ControlEvent::SessionOpened {
                    session_id: "session-1".to_owned(),
                },
            ),
        ];
        for index in 0..total {
            let position = u64::try_from(events.len()).expect("log length fits in u64");
            events.push(CloudEvent::new(
                id.as_str(),
                Version::new(position + 1),
                ControlEvent::UpdateRecorded {
                    update: AgentUpdate::Message {
                        text: format!("u{index}"),
                    },
                },
            ));
        }
        let position = u64::try_from(events.len()).expect("log length fits in u64");
        events.push(CloudEvent::new(
            id.as_str(),
            Version::new(position + 1),
            ControlEvent::Completed {
                summary: "done".to_owned(),
            },
        ));

        let state = TaskState::replay(&events).expect("the legacy log replays");
        assert_eq!(state.status, TaskStatus::Completed);
        assert_eq!(state.session.as_deref(), Some("session-1"));
        assert_eq!(state.update_count, total as u64);
        assert_eq!(state.updates.len(), MAX_UPDATES);
        assert_eq!(
            state.updates.last(),
            Some(&AgentUpdate::Message {
                text: format!("u{}", total - 1),
            })
        );
    }

    #[test]
    fn replay_of_an_empty_stream_is_new() {
        assert_eq!(TaskState::replay(&[]), Ok(TaskState::default()));
    }

    #[test]
    fn replay_rebuilds_a_running_task() {
        let id = TaskId::from_u128(2);
        let events = vec![
            CloudEvent::new(id.as_str(), Version::new(1), submitted()),
            CloudEvent::new(id.as_str(), Version::new(2), ControlEvent::Started),
        ];
        let state = TaskState::replay(&events).expect("the log is well formed");
        assert_eq!(state.status, TaskStatus::Running);
        assert_eq!(state.spec, Some(spec()));
    }

    #[test]
    fn replay_rejects_skipped_versions() {
        let id = TaskId::from_u128(1);
        let events = vec![
            CloudEvent::new(id.as_str(), Version::new(1), submitted()),
            CloudEvent::new(id.as_str(), Version::new(3), ControlEvent::Started),
        ];
        assert!(matches!(
            TaskState::replay(&events),
            Err(ControlError::CorruptHistory(_))
        ));
    }

    #[test]
    fn replay_rejects_mixed_streams() {
        let first = TaskId::from_u128(1);
        let second = TaskId::from_u128(2);
        let events = vec![
            CloudEvent::new(first.as_str(), Version::new(1), submitted()),
            CloudEvent::new(second.as_str(), Version::new(2), ControlEvent::Started),
        ];
        assert!(matches!(
            TaskState::replay(&events),
            Err(ControlError::CorruptHistory(_))
        ));
    }

    #[test]
    fn replay_rejects_an_event_the_state_cannot_take() {
        let id = TaskId::from_u128(1);
        let events = vec![CloudEvent::new(
            id.as_str(),
            Version::new(1),
            ControlEvent::Started,
        )];
        assert!(matches!(
            TaskState::replay(&events),
            Err(ControlError::CorruptHistory(_))
        ));
    }

    #[test]
    fn corrupt_history_names_the_state_and_the_event() {
        let mut state = TaskState::default();
        let error = evolve(&mut state, &ControlEvent::Started).expect_err("new cannot start");
        let message = error.to_string();
        assert!(message.contains("new"), "{message}");
        assert!(message.contains("started"), "{message}");
    }

    #[test]
    fn every_control_error_has_a_message() {
        let cases = [
            (ControlError::InvalidTaskId("bad id".to_owned()), "bad id"),
            (
                ControlError::InvalidSpec {
                    field: "title",
                    reason: "must not be empty".to_owned(),
                },
                "title must not be empty",
            ),
            (ControlError::NotSubmitted, "not submitted"),
            (ControlError::AlreadySubmitted, "already submitted"),
            (ControlError::AlreadyStarted, "already started"),
            (ControlError::NotRunning, "not running"),
            (ControlError::SessionAlreadyOpen, "session"),
            (ControlError::Terminal, "terminal"),
            (ControlError::CorruptHistory("stale".to_owned()), "stale"),
        ];
        for (error, expected) in cases {
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn every_event_names_its_cloud_events_type() {
        let cases = [
            (submitted(), "dev.mokmok.control.submitted"),
            (ControlEvent::Started, "dev.mokmok.control.started"),
            (
                ControlEvent::SessionOpened {
                    session_id: "session-1".to_owned(),
                },
                "dev.mokmok.control.session_opened",
            ),
            (
                ControlEvent::UpdateRecorded { update: message() },
                "dev.mokmok.control.update_recorded",
            ),
            (
                ControlEvent::Completed {
                    summary: "done".to_owned(),
                },
                "dev.mokmok.control.completed",
            ),
            (
                ControlEvent::Failed {
                    reason: "broken".to_owned(),
                },
                "dev.mokmok.control.failed",
            ),
            (
                ControlEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
                "dev.mokmok.control.cancelled",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.event_type(), expected);
        }
    }

    #[test]
    fn every_control_event_round_trips_through_json() {
        for event in [
            submitted(),
            ControlEvent::Started,
            ControlEvent::SessionOpened {
                session_id: "session-1".to_owned(),
            },
            ControlEvent::UpdateRecorded {
                update: AgentUpdate::ToolCall {
                    title: "read".to_owned(),
                    status: "ok".to_owned(),
                },
            },
            ControlEvent::Completed {
                summary: "done".to_owned(),
            },
            ControlEvent::Failed {
                reason: "broken".to_owned(),
            },
            ControlEvent::Cancelled {
                reason: "stop".to_owned(),
            },
        ] {
            let json = serde_json::to_string(&event).expect("event encodes");
            let decoded: ControlEvent = serde_json::from_str(&json).expect("event decodes");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn a_task_state_round_trips_through_json() {
        let mut state = reached(TaskStatus::Running);
        evolve(
            &mut state,
            &ControlEvent::SessionOpened {
                session_id: "session-1".to_owned(),
            },
        )
        .expect("the first session opens");
        evolve(
            &mut state,
            &ControlEvent::UpdateRecorded {
                update: AgentUpdate::Thought {
                    text: "thinking".to_owned(),
                },
            },
        )
        .expect("an update follows running");
        let json = serde_json::to_string(&state).expect("state encodes");
        let decoded: TaskState = serde_json::from_str(&json).expect("state decodes");
        assert_eq!(decoded, state);
        assert!(json.contains("\"status\":\"running\""), "{json}");
        assert!(json.contains("\"kind\":\"thought\""), "{json}");
    }

    #[test]
    fn every_status_serializes_in_snake_case() {
        for (status, expected) in [
            (TaskStatus::New, "\"new\""),
            (TaskStatus::Submitted, "\"submitted\""),
            (TaskStatus::Running, "\"running\""),
            (TaskStatus::Completed, "\"completed\""),
            (TaskStatus::Failed, "\"failed\""),
            (TaskStatus::Cancelled, "\"cancelled\""),
        ] {
            assert_eq!(
                serde_json::to_string(&status).expect("status encodes"),
                expected
            );
        }
    }
}

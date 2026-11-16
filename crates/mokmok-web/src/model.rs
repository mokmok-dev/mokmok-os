//! The shape a page renders: workspace groups, the selected session, and its turns.
//!
//! A page builds this once per request from the port, so the view code reads plain data
//! and holds no control-plane handles. Every string is owned because a Topcoat view must
//! outlive its handler frame.

use mokmok_control_core::TaskStatus;
use mokmok_journal::JournalEvent;

use crate::port::{Entry, Session, title_from_instruction};

/// The label a workspace with no path gets.
pub const NO_WORKSPACE_LABEL: &str = "No workspace";

/// The largest workspace path the domain accepts.
pub const MAX_WORKSPACE_BYTES: usize = 4096;

/// The newest transcript entries a page asks for.
pub const TRANSCRIPT_LIMIT: usize = 1000;

/// One workspace and its sessions, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// The normalized path, or empty for the group with no workspace.
    pub key: String,
    pub path: Option<String>,
    /// The last path segment, or [`NO_WORKSPACE_LABEL`].
    pub label: String,
    pub sessions: Vec<SessionRow>,
}

/// A session as the sidebar lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    /// How long ago the task was submitted, such as `3m ago`.
    pub age: String,
    /// Whether the row is the selected session.
    pub selected: bool,
}

/// A session's full turn in the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub task_id: String,
    pub title: String,
    pub status: TaskStatus,
    /// The instruction as the user wrote it, previewed to the listing budget.
    pub instruction: String,
    /// Whether [`Self::instruction`] is a preview of a longer instruction.
    pub instruction_truncated: bool,
    pub parts: Vec<Part>,
    /// The failure reason or cancellation reason, when there is one.
    pub outcome: Option<String>,
    /// Whether this turn belongs to the selected session.
    pub selected: bool,
}

/// One entry of an assistant turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The journal sequence number, which is unique within a task.
    pub seq: u64,
    pub kind: PartKind,
    /// The tool or permission title for a call, or the text for a message.
    pub title: String,
    /// The status of a tool call, as the agent reported it.
    pub status: String,
}

/// Which kind of transcript entry a [`Part`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Message,
    Thought,
    ToolCall,
    Permission,
}

impl PartKind {
    /// Whether the part renders as a card rather than prose.
    #[must_use]
    pub const fn is_call(self) -> bool {
        matches!(self, Self::ToolCall | Self::Permission)
    }
}

/// The label and theme color a status shows as a badge or dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Idle,
    Active,
    Success,
    Danger,
    Warning,
}

impl Tone {
    /// The CSS modifier a dot or badge carries for this tone.
    #[must_use]
    pub const fn css(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::Success => "success",
            Self::Danger => "danger",
            Self::Warning => "warning",
        }
    }
}

/// The label and tone a task status renders as.
#[must_use]
pub const fn status_label(status: TaskStatus) -> (&'static str, Tone) {
    match status {
        TaskStatus::New => ("New", Tone::Idle),
        TaskStatus::Submitted => ("Queued", Tone::Active),
        TaskStatus::Running => ("Running", Tone::Active),
        TaskStatus::Completed => ("Completed", Tone::Success),
        TaskStatus::Failed => ("Failed", Tone::Danger),
        TaskStatus::Cancelled => ("Cancelled", Tone::Warning),
        TaskStatus::Interrupted => ("Interrupted", Tone::Warning),
    }
}

/// Whether a task is queued or running, so a page keeps watching it.
///
/// A status this does not name is not active, which is the safe answer for one a page
/// would otherwise poll forever.
#[must_use]
pub const fn is_active(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Submitted | TaskStatus::Running)
}

/// The normalized form of a workspace path, so two spellings group together.
#[must_use]
pub fn normalize_workspace(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The sidebar label of a workspace path.
#[must_use]
pub fn workspace_label(path: Option<&str>) -> String {
    let Some(path) = path else {
        return NO_WORKSPACE_LABEL.to_owned();
    };
    let normalized = normalize_workspace(path);
    if normalized == "/" {
        return "/".to_owned();
    }
    normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_owned()
}

/// Why a workspace path is not usable, or `None` when it is.
#[must_use]
pub fn validate_workspace(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return Some("The workspace must be an absolute path.".to_owned());
    }
    if path.len() > MAX_WORKSPACE_BYTES {
        return Some(format!(
            "The workspace path must be at most {MAX_WORKSPACE_BYTES} bytes."
        ));
    }
    if path.contains('\0') {
        return Some("The workspace path must not contain NUL.".to_owned());
    }
    if path.contains('\n') || path.contains('\r') {
        return Some("The workspace path must not contain a line break.".to_owned());
    }
    None
}

/// Orders groups by path with the no-workspace group last.
///
/// Saved paths keep the order the caller lists them in, and a discovered path follows
/// them sorted, which is what makes a newly used workspace appear in a stable place.
/// Sessions keep the order the port delivered them in, which is newest first; a sort is
/// stable, so a port that delivers them oldest first shows its own age order instead.
#[must_use]
pub fn group_sessions(
    sessions: Vec<Session>,
    saved: &[String],
    now: u64,
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for path in saved {
        let key = normalize_workspace(path);
        if groups.iter().any(|group| group.key == key) {
            continue;
        }
        groups.push(Group {
            key: key.clone(),
            path: Some(key.clone()),
            label: workspace_label(Some(&key)),
            sessions: Vec::new(),
        });
    }
    let saved_count = groups.len();
    for session in sessions {
        let key = session
            .workspace
            .as_deref()
            .map_or_else(String::new, normalize_workspace);
        if let Some(group) = groups.iter_mut().find(|group| group.key == key) {
            group.sessions.push(session_row(&session, now));
            continue;
        }
        groups.push(Group {
            key: key.clone(),
            path: session.workspace.as_deref().map(|_| key.clone()),
            label: workspace_label(session.workspace.as_deref()),
            sessions: vec![session_row(&session, now)],
        });
    }
    let mut discovered: Vec<Group> = groups.split_off(saved_count);
    discovered.sort_by(|left, right| compare_paths(left.path.as_deref(), right.path.as_deref()));
    groups.extend(discovered);
    groups
}

fn compare_paths(
    left: Option<&str>,
    right: Option<&str>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(right),
        // The group with no workspace sorts after every named one.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn session_row(
    session: &Session,
    now: u64,
) -> SessionRow {
    SessionRow {
        id: session.id.as_str().to_owned(),
        title: if session.title.trim().is_empty() {
            title_from_instruction(&session.instruction)
        } else {
            session.title.clone()
        },
        status: session.status,
        age: relative_time(session.submitted_at, now),
        selected: false,
    }
}

/// How long ago a task was submitted, in the coarsest unit that fits.
#[must_use]
pub fn relative_time(
    created: Option<u64>,
    now: u64,
) -> String {
    let Some(created) = created else {
        return "unknown".to_owned();
    };
    let seconds = now.saturating_sub(created);
    if seconds < 60 {
        return "just now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// Builds one turn from a session and its transcript entries.
#[must_use]
pub fn turn(
    session: &Session,
    entries: &[Entry],
    selected: bool,
) -> Turn {
    Turn {
        task_id: session.id.as_str().to_owned(),
        title: session.title.clone(),
        status: session.status,
        instruction: session.instruction.clone(),
        instruction_truncated: session.instruction_truncated,
        parts: entries
            .iter()
            .map(|entry| {
                let (kind, title, status) = describe(&entry.event);
                Part {
                    seq: entry.seq,
                    kind,
                    title,
                    status,
                }
            })
            .collect(),
        outcome: session.outcome.clone(),
        selected,
    }
}

/// The kind, title, and status a journal event renders as.
fn describe(event: &JournalEvent) -> (PartKind, String, String) {
    match event {
        JournalEvent::Message { text } => (PartKind::Message, text.clone(), String::new()),
        JournalEvent::Thought { text } => (PartKind::Thought, text.clone(), String::new()),
        JournalEvent::ToolCall { title, status } => {
            (PartKind::ToolCall, title.clone(), status.clone())
        },
        JournalEvent::Permission { title, status } => {
            (PartKind::Permission, title.clone(), status.clone())
        },
    }
}

/// The state a tool call shows, from the status the agent reported.
#[must_use]
pub fn call_state(status: &str) -> &'static str {
    match status {
        "completed" => "complete",
        "failed" | "error" => "error",
        _ => "input-complete",
    }
}

#[cfg(test)]
mod tests {
    use mokmok_control_core::TaskId;

    use super::*;

    fn now() -> u64 {
        1_700_000_000
    }

    fn session(
        id: &str,
        workspace: Option<&str>,
    ) -> Session {
        Session {
            id: TaskId::parse(id).expect("a task id"),
            title: format!("title {id}"),
            instruction: "instruction".to_owned(),
            instruction_truncated: false,
            status: TaskStatus::Completed,
            agent: "local".to_owned(),
            workspace: workspace.map(str::to_owned),
            outcome: None,
            submitted_at: None,
            updates: None,
        }
    }

    #[test]
    fn a_workspace_label_is_its_last_segment() {
        assert_eq!(workspace_label(Some("/srv/mokmok")), "mokmok");
        assert_eq!(workspace_label(Some("/srv/mokmok/")), "mokmok");
        assert_eq!(workspace_label(Some("/")), "/");
        assert_eq!(workspace_label(None), NO_WORKSPACE_LABEL);
    }

    #[test]
    fn saved_workspaces_come_first_and_keep_their_order() {
        let saved = vec!["/b".to_owned(), "/a".to_owned()];
        let groups = group_sessions(vec![], &saved, now());
        assert_eq!(
            groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["/b", "/a"]
        );
    }

    #[test]
    fn a_discovered_workspace_follows_the_saved_ones_sorted() {
        let saved = vec!["/z".to_owned()];
        let id = TaskId::from_u128(1).as_str().to_owned();
        let groups = group_sessions(
            vec![session(&id, Some("/b")), session(&id, Some("/a"))],
            &saved,
            now(),
        );
        assert_eq!(
            groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["/z", "/a", "/b"]
        );
    }

    #[test]
    fn the_group_with_no_workspace_sorts_last() {
        let id = TaskId::from_u128(1).as_str().to_owned();
        let groups = group_sessions(
            vec![session(&id, None), session(&id, Some("/a"))],
            &[],
            now(),
        );
        let keys: Vec<_> = groups.iter().map(|group| group.key.as_str()).collect();
        assert_eq!(keys, vec!["/a", ""]);
    }

    #[test]
    fn a_trailing_slash_joins_the_same_group() {
        let saved = vec!["/a/".to_owned()];
        let id = TaskId::from_u128(1).as_str().to_owned();
        let groups = group_sessions(vec![session(&id, Some("/a"))], &saved, now());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].sessions.len(), 1);
    }

    #[test]
    fn a_status_has_a_label_and_a_tone() {
        assert_eq!(status_label(TaskStatus::Running), ("Running", Tone::Active));
        assert_eq!(status_label(TaskStatus::Failed).1, Tone::Danger);
        assert_eq!(
            status_label(TaskStatus::Interrupted),
            ("Interrupted", Tone::Warning)
        );
        assert_eq!(Tone::Warning.css(), "warning");
    }

    #[test]
    fn only_queued_and_running_tasks_are_active() {
        assert!(is_active(TaskStatus::Submitted));
        assert!(is_active(TaskStatus::Running));
        assert!(!is_active(TaskStatus::Completed));
        assert!(!is_active(TaskStatus::New));
        assert!(
            !is_active(TaskStatus::Interrupted),
            "a leftover run stopped"
        );
    }

    #[test]
    fn a_group_keeps_the_order_the_port_delivered() {
        let older = session("00000000-0000-0000-0000-00000000000a", Some("/w"));
        let newer = session("00000000-0000-0000-0000-00000000000b", Some("/w"));
        let groups = group_sessions(
            vec![
                Session {
                    submitted_at: Some(1_700_000_100),
                    ..newer
                },
                Session {
                    submitted_at: Some(1_700_000_000),
                    ..older
                },
            ],
            &[],
            now(),
        );
        let listed: Vec<&str> = groups[0]
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![
                "00000000-0000-0000-0000-00000000000b",
                "00000000-0000-0000-0000-00000000000a"
            ],
            "the port's newest-first order is what a group shows"
        );
    }

    #[test]
    fn a_session_age_comes_from_the_first_record_not_the_id() {
        // A client-named task is a v5, which carries no readable time of its own.
        let mut row = session("041ed7ad-db63-5e84-8d95-23b4dea3e462", Some("/w"));
        row.submitted_at = Some(now() - 120);
        let groups = group_sessions(vec![row], &[], now());
        assert_eq!(groups[0].sessions[0].age, "2m ago");
    }

    #[test]
    fn relative_time_uses_the_coarsest_unit_that_fits() {
        let base = now();
        assert_eq!(relative_time(Some(base), base), "just now");
        assert_eq!(relative_time(Some(base - 60), base), "1m ago");
        assert_eq!(relative_time(Some(base - 7_200), base), "2h ago");
        assert_eq!(relative_time(Some(base - 345_600), base), "4d ago");
        assert_eq!(relative_time(None, base), "unknown");
    }

    #[test]
    fn relative_time_switches_unit_at_the_exact_boundary() {
        let base = now();
        // One second short of a minute stays in seconds, and a minute exactly moves up.
        assert_eq!(relative_time(Some(base - 59), base), "just now");
        assert_eq!(relative_time(Some(base - 60), base), "1m ago");
        // The same two boundaries for minutes into hours.
        assert_eq!(relative_time(Some(base - 3_599), base), "59m ago");
        assert_eq!(relative_time(Some(base - 3_600), base), "1h ago");
        // And for hours into days.
        assert_eq!(relative_time(Some(base - 86_399), base), "23h ago");
        assert_eq!(relative_time(Some(base - 86_400), base), "1d ago");
    }

    #[test]
    fn a_workspace_must_be_absolute_and_free_of_control_bytes() {
        assert_eq!(validate_workspace("/w"), None);
        assert!(validate_workspace("w").is_some());
        assert!(validate_workspace("/w\n").is_some());
        assert!(validate_workspace("/w\0").is_some());
    }

    #[test]
    fn a_workspace_at_the_byte_budget_is_accepted() {
        let exact = format!("/{}", "a".repeat(MAX_WORKSPACE_BYTES - 1));
        assert_eq!(exact.len(), MAX_WORKSPACE_BYTES);
        assert_eq!(validate_workspace(&exact), None);

        let over = format!("/{}", "a".repeat(MAX_WORKSPACE_BYTES));
        assert_eq!(over.len(), MAX_WORKSPACE_BYTES + 1);
        assert_eq!(
            validate_workspace(&over),
            Some(format!(
                "The workspace path must be at most {MAX_WORKSPACE_BYTES} bytes."
            ))
        );
    }

    #[test]
    fn a_message_part_carries_its_text_and_a_call_part_its_status() {
        let text = describe(&JournalEvent::Message {
            text: "hello".to_owned(),
        });
        assert_eq!(text, (PartKind::Message, "hello".to_owned(), String::new()));
        let call = describe(&JournalEvent::ToolCall {
            title: "read".to_owned(),
            status: "completed".to_owned(),
        });
        assert_eq!(call.0, PartKind::ToolCall);
        assert!(call.0.is_call());
        assert!(!PartKind::Thought.is_call());
    }

    #[test]
    fn a_call_state_maps_the_agent_status() {
        assert_eq!(call_state("completed"), "complete");
        assert_eq!(call_state("failed"), "error");
        assert_eq!(call_state("error"), "error");
        assert_eq!(call_state("in_progress"), "input-complete");
    }
}

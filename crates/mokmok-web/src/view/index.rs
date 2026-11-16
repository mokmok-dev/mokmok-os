//! The screens, built from Topcoat components.
//!
//! Every renderable piece is a `#[component]` so it nests directly in `view!` and inherits
//! the request context. A component takes owned props because a view outlives the
//! handler's borrows.

use mokmok_control_core::TaskStatus;
use topcoat::{
    Result,
    context::Cx,
    view::{View, class, component, view},
};

use crate::{
    model::{Group, Tone, Turn, status_label},
    port::Agents,
};

/// The sidebar: the brand, the theme toggle, and every workspace group.
#[component]
pub async fn sidebar(
    cx: &Cx,
    groups: Vec<Group>,
    theme: String,
) -> Result<impl View> {
    let empty = groups.is_empty();
    // The link names the theme it switches to, so its label is the action it takes.
    let toggle = if theme == "light" { "dark" } else { "light" };
    let toggle_label = if theme == "light" { "Dark" } else { "Light" };
    Ok(view! {
        <aside class="sidebar" aria-label="Sessions">
            <div class="sidebar-header">
                <div class="brand">
                    <span class="brand-mark" aria-hidden="true">"m"</span>
                    <span class="brand-text">
                        <span class="brand-name">"mokmok"</span>
                        <span class="brand-subtitle">"control plane"</span>
                    </span>
                </div>
                <a class="button ghost" href=(format!("/?theme={toggle}"))>
                    (toggle_label)
                </a>
            </div>
            <nav class="sidebar-content">
                for group in groups {
                    group_section(group: group, theme: theme.clone())
                }
                if empty {
                    <p class="sidebar-empty">"No sessions yet"</p>
                }
            </nav>
        </aside>
    })
}

/// One workspace and its sessions.
#[component]
async fn group_section(
    cx: &Cx,
    group: Group,
    theme: String,
) -> Result<impl View> {
    let path = group.path.clone().unwrap_or_default();
    let count = group.sessions.len();
    let empty = group.sessions.is_empty();
    let add_href = format!("/?workspace={}&theme={theme}", super::encode(&group.key));
    let key = group.key.clone();
    let label = group.label.clone();
    Ok(view! {
        <section class="group">
            <h2 class="group-label" title=(path)>
                <span class="group-text">(label)</span>
                <span class="group-count">(count)</span>
            </h2>
            <ul class="session-list">
                for session in group.sessions {
                    <li>
                        <a
                            class="session-item"
                            href=(format!(
                                "/?workspace={}&session={}&theme={theme}",
                                super::encode(&key),
                                super::encode(&session.id),
                            ))
                            aria-current=(session.selected.then_some("page"))
                        >
                            status_dot(status: session.status)
                            <span class="session-text">
                                <span class="session-title">(session.title)</span>
                                <span class="session-meta">(session.age)</span>
                            </span>
                        </a>
                    </li>
                }
            </ul>
            if empty {
                <a class="group-empty" href=(add_href)>"Add a session"</a>
            }
        </section>
    })
}

/// The header above the transcript.
#[component]
pub async fn main_header(
    cx: &Cx,
    workspace: String,
    title: String,
    task: String,
    status: Option<TaskStatus>,
    active: bool,
    theme: String,
) -> Result<impl View> {
    let cancel_workspace = workspace.clone();
    Ok(view! {
        <header class="main-header">
            <div class="header-title">
                <span class="workspace-path">(workspace)</span>
                <h1 class="session-heading">(title)</h1>
            </div>
            <div class="header-actions">
                if let Some(status) = status {
                    status_badge(status: status)
                }
                if active {
                    <form method="post" action="/sessions/cancel" class="inline-form">
                        <input type="hidden" name="task" value=(task)>
                        <input type="hidden" name="theme" value=(theme)>
                        <input
                            type="hidden"
                            name="workspace"
                            value=(cancel_workspace)
                        >
                        <button type="submit" class="button secondary">"Cancel"</button>
                    </form>
                }
            </div>
        </header>
    })
}

/// The whole transcript: the turns, and a note while a session is running.
#[component]
pub async fn chat(
    cx: &Cx,
    turns: Vec<Turn>,
    live_task: Option<String>,
) -> Result<impl View> {
    let empty = turns.is_empty();
    Ok(view! {
        <section class="chat-pane" aria-label="Transcript">
            <div class="chat-scroll">
                <p class="thread-label">"Workspace thread"</p>
                if empty {
                    // A group with sessions always selects one, so an empty pane means the
                    // workspace has no session yet.
                    <div class="empty-state">
                        <h2>"No sessions yet"</h2>
                        <p>"Send an instruction to start a session in this workspace."</p>
                    </div>
                }
                <div class="turn-list">
                    for current in turns.clone() {
                        turn_view(turn: current)
                    }
                </div>
                if live_task.is_some() {
                    <p class="live-note" role="status">
                        "Following the running session. The page refreshes every 2 seconds."
                    </p>
                }
            </div>
        </section>
    })
}

/// One turn: the instruction beside the assistant's transcript.
#[component]
pub async fn turn_view(
    cx: &Cx,
    turn: Turn,
) -> Result<impl View> {
    let parts = turn.parts.clone();
    let shows_outcome = turn.status.is_terminal() && turn.status != TaskStatus::Completed;
    Ok(view! {
        <article
            class="turn"
            data-selected=(if turn.selected { "true" } else { "false" })
            data-task=(turn.task_id)
        >
            <div class="bubble">
                <p class="text-part">(turn.instruction)</p>
                if turn.instruction_truncated {
                    <p class="muted preview-note">
                        "Instruction preview; the agent received the full text."
                    </p>
                }
            </div>
            <div class="assistant-body">
                if parts.is_empty() {
                    <p class="muted">"No transcript yet."</p>
                }
                for part in parts {
                    if part.kind.is_call() {
                        <div class="tool-card">
                            <div class="tool-card-head">
                                <span class="tool-name">(part.title)</span>
                                <span
                                    class="tool-state"
                                    data-state=(crate::model::call_state(&part.status))
                                >
                                    (part.status)
                                </span>
                            </div>
                            <p class="tool-kind">(kind_label(part.kind))</p>
                        </div>
                    } else {
                        <p class=(class!("text-part", "thought-part" if part.kind == crate::model::PartKind::Thought))>
                            (part.title)
                        </p>
                    }
                }
                if shows_outcome {
                    if let Some(outcome) = turn.outcome.clone() {
                        <p class=(class!("outcome-note", "outcome-warning" if status_label(turn.status).1 == Tone::Warning))>
                            (outcome)
                        </p>
                    }
                }
            </div>
        </article>
    })
}

/// The region a running session keeps current.
///
/// The page carries a meta refresh while a session runs, so the whole document re-reads
/// itself and this region arrives already current. It is marked live for a screen reader,
/// which announces the transcript as it grows.
#[component]
pub async fn live_region(
    cx: &Cx,
    turns: Vec<Turn>,
) -> Result<impl View> {
    let _ = cx;
    Ok(view! {
        <div class="live-region" aria-live="polite">
            for current in turns {
                turn_view(turn: current)
            }
        </div>
    })
}

/// The composer that starts a new session.
#[component]
pub async fn composer(
    cx: &Cx,
    agents: Agents,
    workspace: String,
    theme: String,
) -> Result<impl View> {
    let default = agents.default.clone();
    let theme_value = theme;
    // Minted per render, so a resubmitted form or a double Send addresses the same task
    // instead of starting a second one.
    let request_id = uuid::Uuid::now_v7().to_string();
    Ok(view! {
        <form method="post" action="/sessions" class="composer">
            <div class="composer-inner">
                <input type="hidden" name="theme" value=(theme_value)>
                <input type="hidden" name="request_id" value=(request_id)>
                <label class="visually-hidden" for="composer-input">"Instruction"</label>
                <textarea
                    id="composer-input"
                    name="instruction"
                    rows="3"
                    placeholder="Delegate an instruction..."
                    required="required"
                ></textarea>
                <div class="composer-actions">
                    <label class="workspace-field" for="composer-workspace">
                        <span class="workspace-label">"Workspace"</span>
                        <input
                            id="composer-workspace"
                            name="workspace"
                            value=(workspace)
                            placeholder="/absolute/path"
                            spellcheck="false"
                        >
                    </label>
                    <label class="visually-hidden" for="agent-select">"Agent"</label>
                    <select id="agent-select" name="agent">
                        for name in agents.names {
                            <option
                                value=(name.clone())
                                selected=(if name == default { Some("selected") } else { None })
                            >
                                (name)
                            </option>
                        }
                    </select>
                    <button type="submit" class="button primary">"Send"</button>
                </div>
                <p class="composer-hint">
                    "Each session delegates one task in the workspace above. A follow-up starts a new session in the same workspace."
                </p>
            </div>
        </form>
    })
}

/// The dot beside a session row, colored by its status.
#[component]
async fn status_dot(
    cx: &Cx,
    status: TaskStatus,
) -> Result<impl View> {
    Ok(view! {
        <span class=(class!("status-dot", tone_class(status_label(status).1)))></span>
    })
}

/// The badge in the header, colored by the status.
#[component]
async fn status_badge(
    cx: &Cx,
    status: TaskStatus,
) -> Result<impl View> {
    let (label, tone) = status_label(status);
    Ok(view! { <span class=(class!("badge", tone_class(tone)))>(label)</span> })
}

/// The CSS modifier a tone adds to a dot or badge.
#[must_use]
pub const fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Idle => "tone-idle",
        Tone::Active => "tone-active",
        Tone::Success => "tone-success",
        Tone::Danger => "tone-danger",
        Tone::Warning => "tone-warning",
    }
}

#[must_use]
pub const fn kind_label(kind: crate::model::PartKind) -> &'static str {
    match kind {
        crate::model::PartKind::Message => "message",
        crate::model::PartKind::Thought => "thought",
        crate::model::PartKind::ToolCall => "tool call",
        crate::model::PartKind::Permission => "permission",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_its_own_class() {
        assert_eq!(tone_class(Tone::Idle), "tone-idle");
        assert_eq!(tone_class(Tone::Active), "tone-active");
        assert_eq!(tone_class(Tone::Success), "tone-success");
        assert_eq!(tone_class(Tone::Danger), "tone-danger");
        assert_eq!(tone_class(Tone::Warning), "tone-warning");
    }

    #[test]
    fn a_query_value_is_percent_encoded() {
        assert_eq!(super::super::encode("/srv/mokmok"), "/srv/mokmok");
        assert_eq!(super::super::encode("a b&c"), "a%20b%26c");
        assert_eq!(super::super::encode("é"), "%C3%A9");
    }

    #[test]
    fn a_call_kind_is_labelled_for_a_human() {
        assert_eq!(kind_label(crate::model::PartKind::Permission), "permission");
    }
}

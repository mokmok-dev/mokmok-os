//! The index page: reads the control plane, then assembles the components.

use mokmok_control_core::TaskStatus;
use topcoat::{
    Result,
    context::Cx,
    view::{View, view},
};

use crate::{
    IndexQuery,
    model::{
        Group, NO_WORKSPACE_LABEL, TRANSCRIPT_LIMIT, Turn, group_sessions, is_active,
        normalize_workspace, turn,
    },
    port::{Agents, PortError},
    view::{index, plane_arc},
};

/// Renders the whole index document.
///
/// # Errors
///
/// Returns the view error Topcoat raises when a component's rendering fails.
pub async fn render(
    cx: &Cx,
    query: IndexQuery,
) -> Result<impl View> {
    let board = Board::load(cx, &query).await?;
    Ok(board.render(cx))
}

/// Everything the page renders, read from the port once per request.
pub struct Board {
    pub theme: String,
    pub groups: Vec<Group>,
    pub workspace: String,
    pub workspace_path: String,
    pub title: String,
    pub task: String,
    pub status: Option<TaskStatus>,
    pub active: bool,
    pub turns: Vec<Turn>,
    pub agents: Agents,
    pub error: Option<String>,
}

impl Board {
    /// Reads the control plane and builds everything the page renders.
    ///
    /// A failed read still yields a board, because the page renders the failure in its
    /// banner instead of dropping the whole screen.
    ///
    /// # Errors
    ///
    /// Returns when the request context carries no control-plane port.
    async fn load(
        cx: &Cx,
        query: &IndexQuery,
    ) -> Result<Self, PortError> {
        let plane = plane_arc(cx)?;
        let agents = plane.agents();
        let theme = if query.theme.as_deref() == Some("light") {
            "light".to_owned()
        } else {
            "dark".to_owned()
        };
        let workspace = query
            .workspace
            .as_deref()
            .map(normalize_workspace)
            .unwrap_or_default();
        let sessions = match plane.sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                return Ok(Self {
                    theme,
                    groups: Vec::new(),
                    workspace,
                    workspace_path: String::new(),
                    title: "New session".to_owned(),
                    task: String::new(),
                    status: None,
                    active: false,
                    turns: Vec::new(),
                    agents,
                    error: Some(error.to_string()),
                });
            },
        };
        let mut groups = group_sessions(sessions.clone(), &[], now_seconds());
        // A query with no workspace shows the first group, so a fresh visit lands on a
        // session instead of an empty pane.
        let asked = workspace;
        let shown = groups
            .iter()
            .find(|group| group.key == asked)
            .or_else(|| groups.first())
            .map_or_else(String::new, |group| group.key.clone());
        let workspace_path = groups
            .iter()
            .find(|group| group.key == shown)
            .and_then(|group| group.path.clone())
            .unwrap_or_default();
        let selected_id = query.session.clone().or_else(|| {
            groups
                .iter()
                .find(|group| group.key == shown)
                .and_then(|group| group.sessions.first())
                .map(|session| session.id.clone())
        });
        for group in &mut groups {
            for session in &mut group.sessions {
                session.selected = selected_id.as_deref() == Some(session.id.as_str());
            }
        }
        let selected = selected_id
            .as_deref()
            .and_then(|id| sessions.iter().find(|session| session.id.as_str() == id));
        let mut turns = Vec::new();
        let mut error = query.error.clone();
        if let Some(session) = selected {
            match plane.transcript(session.id.clone(), TRANSCRIPT_LIMIT).await {
                Ok(transcript) => turns.push(turn(session, &transcript.entries, true)),
                Err(failure) => error = Some(failure.to_string()),
            }
        }
        Ok(Self {
            theme,
            groups,
            workspace: shown,
            workspace_path,
            title: selected
                .map_or_else(|| "New session".to_owned(), |session| session.title.clone()),
            task: selected.map_or_else(String::new, |session| session.id.as_str().to_owned()),
            status: selected.map(|session| session.status),
            active: selected.is_some_and(|session| is_active(session.status)),
            turns,
            agents,
            error,
        })
    }

    fn render(
        self,
        cx: &Cx,
    ) -> impl View {
        let live_task = self
            .active
            .then(|| self.task.clone())
            .filter(|task| !task.is_empty());
        let path = if self.workspace.is_empty() {
            NO_WORKSPACE_LABEL.to_owned()
        } else {
            self.workspace.clone()
        };
        let theme = self.theme.clone();
        let refresh = live_task.as_ref().map(|task| {
            format!(
                "/?workspace={}&session={}&theme={theme}",
                super::encode(&self.workspace),
                super::encode(task),
            )
        });
        view! {
            <!DOCTYPE html>
            <html lang="en" data-mode=(theme)>
                <head>
                    <meta charset="utf-8">
                    <meta name="viewport" content="width=device-width, initial-scale=1">
                    <title>"mokmok control plane"</title>
                    <link rel="stylesheet" href="/app.css">
                    <link rel="icon" href="/favicon.svg" type="image/svg+xml">
                    if let Some(href) = refresh {
                        // A running session re-reads itself, so the transcript advances
                        // without a held-open response or a client build step.
                        <meta http-equiv="refresh" content=(format!("2; url={href}"))>
                    }
                </head>
                <body>
                    <a class="skip-link" href="#main">"Skip to main content"</a>
                    <div class="app-shell">
                        index::sidebar(groups: self.groups.clone(), theme: self.theme.clone())
                        <main class="main-pane" id="main" tabindex="-1">
                            index::main_header(
                                workspace: path,
                                title: self.title.clone(),
                                task: self.task.clone(),
                                status: self.status,
                                active: self.active,
                                theme: self.theme.clone(),
                            )
                            if let Some(error) = self.error.clone() {
                                <div class="banner" role="alert">
                                    <p class="banner-title">"The control plane refused the request"</p>
                                    <p class="banner-body">(error)</p>
                                </div>
                            }
                            index::chat(
                                turns: self.turns.clone(),
                                live_task: live_task,
                            )
                            index::composer(
                                agents: self.agents.clone(),
                                workspace: self.workspace_path.clone(),
                                theme: self.theme.clone(),
                            )
                        </main>
                    </div>
                </body>
            </html>
        }
    }
}

/// The clock the sidebar's relative times are measured against.
fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The theme a query asks for, defaulting to dark.
#[must_use]
pub fn theme_of(raw: Option<&str>) -> String {
    if raw == Some("light") {
        "light".to_owned()
    } else {
        "dark".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_board_without_a_theme_reads_dark() {
        assert_eq!(theme_of(None), "dark");
        assert_eq!(theme_of(Some("light")), "light");
        assert_eq!(theme_of(Some("solarized")), "dark");
    }

    #[test]
    fn the_clock_is_the_wall_clock_not_a_constant() {
        // A constant clock dates every session against 1970 or a fixed instant, which the
        // sidebar would show as an absurd age.
        let now = now_seconds();
        assert!(now > 1_700_000_000, "{now}");
        assert!(now < 4_000_000_000, "{now}");
    }
}

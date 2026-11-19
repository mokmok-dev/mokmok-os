//! Server-rendered HTML for the control plan.
//!
//! The pages read a [`ControlPlane`] instead of the daemon's state,
//! so this crate tests against a fake and the daemon decides what the port answers. The
//! [`model`] turns those reads into the owned data a page renders.
//!
//! The browser gets plain HTML and forms. A live transcript region streams over the same
//! response with Topcoat's `live!`, and a form post answers a `303` back to the page it
//! came from, so no client build step is needed.

pub mod model;
pub mod port;
pub mod view;

use std::sync::Arc;

use mokmok_control_core::TaskId;
use topcoat::{
    Result,
    context::Cx,
    router::{
        Router, StatusCode,
        content::Form,
        error::{SeeOther, see_other},
        header::{HeaderMap, HeaderName, HeaderValue},
        page, route,
    },
    view::View,
};

use crate::{
    port::{ControlPlane, NewSession},
    view::{FAVICON, STYLESHEET, encode, page},
};

/// The query the index page reads.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct IndexQuery {
    /// The workspace whose sessions the page shows.
    pub workspace: Option<String>,
    /// The task whose turn is selected.
    pub session: Option<String>,
    /// `light` or `dark`.
    pub theme: Option<String>,
    /// Why the last command failed, shown above the transcript.
    pub error: Option<String>,
}

/// The form a new session posts.
#[derive(Debug, Default, serde::Deserialize)]
pub struct SubmitForm {
    pub instruction: Option<String>,
    pub agent: Option<String>,
    pub workspace: Option<String>,
    /// The value the composer minted and posted back with the instruction.
    pub request_id: Option<String>,
    pub theme: Option<String>,
}

/// The form a cancel posts.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CancelForm {
    pub task: Option<String>,
    pub workspace: Option<String>,
    pub theme: Option<String>,
}

/// Builds the router that serves the control-plane pages.
#[must_use]
pub fn router(plane: Arc<dyn ControlPlane>) -> Router {
    Router::builder()
        .with(plane)
        .page(index)
        .page(stylesheet)
        .page(favicon)
        .route(submit)
        .route(cancel)
        .build()
}

#[page("/")]
async fn index(cx: &Cx) -> Result<impl View> {
    let query = topcoat::router::parse_query_params::<IndexQuery>(cx)
        .map_err(|_| topcoat::router::error::bad_request("the query is not valid"))?;
    page::render(cx, query).await
}

#[page("/app.css")]
async fn stylesheet(cx: &Cx) -> Result<impl View> {
    let _ = cx;
    Ok(topcoat::view::view! {
        (headers("content-type", "text/css; charset=utf-8"))
        (STYLESHEET)
    })
}

#[page("/favicon.svg")]
async fn favicon(cx: &Cx) -> Result<impl View> {
    let _ = cx;
    Ok(topcoat::view::view! {
        (headers("content-type", "image/svg+xml"))
        (FAVICON)
    })
}

/// A one-entry header map, which a page body attaches to its response.
fn headers(
    name: &'static str,
    value: &'static str,
) -> HeaderMap {
    let mut map = HeaderMap::new();
    map.insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
    map
}

#[route(POST "/sessions")]
async fn submit(
    cx: &Cx,
    Form(form): Form<SubmitForm>,
) -> Result<SeeOther> {
    let plane = view::plane_arc(cx)?;
    let theme = page::theme_of(form.theme.as_deref());
    let workspace = form.workspace.clone().unwrap_or_default();
    let instruction = form.instruction.clone().unwrap_or_default();
    if instruction.trim().is_empty() {
        return Ok(redirect(
            &theme,
            &workspace,
            None,
            Some("The instruction is empty."),
        ));
    }
    let agent = resolve_agent(form.agent.as_deref(), &plane.agents().default).to_owned();
    let session = NewSession::from_form(
        &instruction,
        &agent,
        Some(&workspace),
        form.request_id.as_deref(),
    );
    match plane.submit(session).await {
        Ok(id) => Ok(redirect(&theme, &workspace, Some(id.as_str()), None)),
        Err(error) => Ok(redirect(&theme, &workspace, None, Some(&error.to_string()))),
    }
}

#[route(POST "/sessions/cancel")]
async fn cancel(
    cx: &Cx,
    Form(form): Form<CancelForm>,
) -> Result<SeeOther> {
    let plane = view::plane_arc(cx)?;
    let theme = page::theme_of(form.theme.as_deref());
    let workspace = form.workspace.clone().unwrap_or_default();
    let Some(task) = form.task.as_deref().and_then(|raw| TaskId::parse(raw).ok()) else {
        return Ok(redirect(
            &theme,
            &workspace,
            None,
            Some("That task id is not valid."),
        ));
    };
    let failure = plane
        .cancel(task.clone(), "cancelled from the web UI".to_owned())
        .await
        .err()
        .map(|error| error.to_string());
    Ok(redirect(
        &theme,
        &workspace,
        Some(task.as_str()),
        failure.as_deref(),
    ))
}

/// Answers `303` back to the page, carrying the selection and any failure.
fn redirect(
    theme: &str,
    workspace: &str,
    session: Option<&str>,
    error: Option<&str>,
) -> SeeOther {
    see_other(back_to(theme, workspace, session, error))
}

/// The URL a command answers with, so the browser returns to the same view.
fn back_to(
    theme: &str,
    workspace: &str,
    session: Option<&str>,
    error: Option<&str>,
) -> String {
    let mut query = format!("workspace={}&theme={}", encode(workspace), encode(theme));
    if let Some(session) = session {
        query.push_str("&session=");
        query.push_str(&encode(session));
    }
    if let Some(error) = error {
        query.push_str("&error=");
        query.push_str(&encode(error));
    }
    format!("/?{query}")
}

/// The agent a submitted form asks for, falling back to the daemon's default.
///
/// A browser submits every field it draws, so an empty string arrives whenever the form
/// offers no agent at all. Treating it as a name would fail the submission against a table
/// that has no empty entry.
#[must_use]
pub const fn resolve_agent<'a>(
    asked: Option<&'a str>,
    default: &'a str,
) -> &'a str {
    match asked {
        Some(name) if !name.is_empty() => name,
        _ => default,
    }
}

/// The status a port failure answers with, for a page that cannot render.
#[must_use]
pub const fn status_for(error: &port::PortError) -> StatusCode {
    match error {
        port::PortError::NotFound => StatusCode::NOT_FOUND,
        port::PortError::Refused(_) => StatusCode::BAD_REQUEST,
        port::PortError::Finished | port::PortError::Unavailable => {
            StatusCode::INTERNAL_SERVER_ERROR
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_carries_the_selection_and_the_failure() {
        assert_eq!(
            back_to("dark", "/w", Some("id-1"), Some("nope")),
            "/?workspace=/w&theme=dark&session=id-1&error=nope"
        );
    }

    #[test]
    fn a_redirect_without_a_selection_leaves_it_out() {
        assert_eq!(back_to("light", "", None, None), "/?workspace=&theme=light");
    }

    #[test]
    fn a_port_failure_names_its_status() {
        assert_eq!(
            status_for(&port::PortError::NotFound),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for(&port::PortError::Refused("x".to_owned())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for(&port::PortError::Unavailable),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn an_agent_name_wins_and_anything_else_takes_the_default() {
        assert_eq!(resolve_agent(Some("alpha"), "fallback"), "alpha");
        assert_eq!(resolve_agent(Some(""), "fallback"), "fallback");
        assert_eq!(resolve_agent(None, "fallback"), "fallback");
    }

    #[test]
    fn a_header_map_carries_the_one_entry_it_was_given() {
        let map = headers("content-type", "text/css; charset=utf-8");
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/css; charset=utf-8")
        );
    }

    #[tokio::test]
    async fn a_stylesheet_request_answers_with_css() {
        use std::sync::Arc;

        use topcoat::router::{Method, request::Request, tower::TowerService};
        use tower::util::ServiceExt;

        let plane: Arc<dyn ControlPlane> = Arc::new(NoSessions);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/app.css")
            .body(topcoat::router::Body::empty());
        let Ok(request) = request else {
            panic!("the request builds");
        };
        let response = TowerService::new(router(plane))
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/css; charset=utf-8")
        );
    }

    /// A control plane with no tasks, so a route test needs no daemon.
    struct NoSessions;

    impl ControlPlane for NoSessions {
        fn sessions(&self) -> port::Read<'_, Vec<port::Session>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn session_state(
            &self,
            _id: mokmok_control_core::TaskId,
        ) -> port::Read<'_, port::SessionState> {
            Box::pin(async { Err(port::PortError::NotFound) })
        }

        fn transcript(
            &self,
            _id: mokmok_control_core::TaskId,
            _limit: usize,
        ) -> port::Read<'_, port::Transcript> {
            Box::pin(async { Err(port::PortError::NotFound) })
        }

        fn agents(&self) -> port::Agents {
            port::Agents {
                names: vec!["local".to_owned()],
                default: "local".to_owned(),
            }
        }

        fn submit(
            &self,
            _spec: port::NewSession,
        ) -> port::Read<'_, mokmok_control_core::TaskId> {
            Box::pin(async { Ok(mokmok_control_core::TaskId::from_u128(1)) })
        }

        fn cancel(
            &self,
            _id: mokmok_control_core::TaskId,
            _reason: String,
        ) -> port::Read<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn an_empty_agent_field_falls_back_to_the_default_agent() {
        use std::sync::{Arc, Mutex};

        use topcoat::router::{Method, request::Request, tower::TowerService};
        use tower::util::ServiceExt;

        #[derive(Default)]
        struct Recording {
            submitted: Mutex<Vec<String>>,
        }

        impl ControlPlane for Recording {
            fn sessions(&self) -> port::Read<'_, Vec<port::Session>> {
                Box::pin(async { Ok(Vec::new()) })
            }

            fn session_state(
                &self,
                _id: mokmok_control_core::TaskId,
            ) -> port::Read<'_, port::SessionState> {
                Box::pin(async { Err(port::PortError::NotFound) })
            }

            fn transcript(
                &self,
                _id: mokmok_control_core::TaskId,
                _limit: usize,
            ) -> port::Read<'_, port::Transcript> {
                Box::pin(async { Err(port::PortError::NotFound) })
            }

            fn agents(&self) -> port::Agents {
                port::Agents {
                    names: vec!["local".to_owned(), "fallback".to_owned()],
                    default: "fallback".to_owned(),
                }
            }

            fn submit(
                &self,
                spec: port::NewSession,
            ) -> port::Read<'_, mokmok_control_core::TaskId> {
                if let Ok(mut held) = self.submitted.lock() {
                    held.push(spec.agent);
                }
                Box::pin(async { Ok(mokmok_control_core::TaskId::from_u128(1)) })
            }

            fn cancel(
                &self,
                _id: mokmok_control_core::TaskId,
                _reason: String,
            ) -> port::Read<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }

        let plane = Arc::new(Recording::default());
        let request = Request::builder()
            .method(Method::POST)
            .uri("/sessions")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(topcoat::router::Body::from(
                "instruction=do+it&agent=&workspace=%2Fw&theme=dark".to_owned(),
            ));
        let Ok(request) = request else {
            panic!("the request builds");
        };
        let response = TowerService::new(router(Arc::clone(&plane) as Arc<dyn ControlPlane>))
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let submitted = plane
            .submitted
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default();
        assert_eq!(submitted, vec!["fallback".to_owned()]);
    }
}

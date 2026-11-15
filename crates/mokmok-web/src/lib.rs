//! Server-rendered HTML for the control plan.
//!
//! The pages read a [`ControlPlane`](port::ControlPlane) instead of the daemon's state,
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
    let agent = form
        .agent
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| plane.agents().default);
    let session = NewSession::from_form(&instruction, &agent, Some(&workspace));
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
}

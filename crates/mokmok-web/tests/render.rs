//! Rendering tests that drive the real router, so a page's output is what a browser gets.

use std::sync::{Arc, Mutex};

use mokmok_control_core::{INTERRUPTED_REASON, TaskId, TaskStatus};
use mokmok_journal::JournalEvent;
use mokmok_web::{
    port::{
        Agents, ControlPlane, Entry, NewSession, PortError, Read, Session, SessionState, Transcript,
    },
    router,
};
use topcoat::router::{Method, request::Request, response::Response, tower::TowerService};

/// A control plane that answers from memory, so a test needs no daemon.
#[derive(Default)]
struct Fake {
    sessions: Vec<Session>,
    entries: Vec<Entry>,
    agents: Agents,
    submitted: Mutex<Vec<NewSession>>,
    cancelled: Mutex<Vec<String>>,
    fail: bool,
}

impl Fake {
    fn with_session(status: TaskStatus) -> Self {
        Self {
            sessions: vec![Session {
                id: TaskId::from_u128(1),
                title: "first instruction".to_owned(),
                instruction: "first instruction".to_owned(),
                instruction_truncated: false,
                status,
                agent: "local".to_owned(),
                workspace: Some("/srv/one".to_owned()),
                outcome: None,
                submitted_at: None,
                updates: Some(1),
            }],
            entries: vec![Entry {
                seq: 1,
                event: JournalEvent::Message {
                    text: "the agent answered".to_owned(),
                },
            }],
            agents: Agents {
                names: vec!["local".to_owned(), "remote".to_owned()],
                default: "local".to_owned(),
            },
            ..Self::default()
        }
    }

    fn id(&self) -> TaskId {
        self.sessions
            .first()
            .map_or_else(|| TaskId::from_u128(1), |session| session.id.clone())
    }

    fn submitted(&self) -> Vec<NewSession> {
        self.submitted
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    fn cancelled(&self) -> Vec<String> {
        self.cancelled
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }
}

impl ControlPlane for Fake {
    fn sessions(&self) -> Read<'_, Vec<Session>> {
        let answer = if self.fail {
            Err(PortError::Unavailable)
        } else {
            Ok(self.sessions.clone())
        };
        Box::pin(async move { answer })
    }

    fn session_state(
        &self,
        id: TaskId,
    ) -> Read<'_, SessionState> {
        let found = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| SessionState {
                status: session.status,
                outcome: session.outcome.clone(),
            });
        Box::pin(async move { found.ok_or(PortError::NotFound) })
    }

    fn transcript(
        &self,
        _id: TaskId,
        limit: usize,
    ) -> Read<'_, Transcript> {
        let entries: Vec<Entry> = self.entries.iter().take(limit).cloned().collect();
        let total = self.entries.len() as u64;
        Box::pin(async move { Ok(Transcript { entries, total }) })
    }

    fn agents(&self) -> Agents {
        self.agents.clone()
    }

    fn submit(
        &self,
        spec: NewSession,
    ) -> Read<'_, TaskId> {
        if let Ok(mut held) = self.submitted.lock() {
            held.push(spec);
        }
        let id = self.id();
        Box::pin(async move { Ok(id) })
    }

    fn cancel(
        &self,
        id: TaskId,
        _reason: String,
    ) -> Read<'_, ()> {
        if let Ok(mut held) = self.cancelled.lock() {
            held.push(id.as_str().to_owned());
        }
        Box::pin(async move { Ok(()) })
    }
}

fn minted(body: &str) -> Option<String> {
    let (_, rest) = body.split_once("name=\"request_id\" value=\"")?;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_owned())
}

/// Sends a request through the page router, the way the daemon mounts it.
async fn send(
    plane: Arc<dyn ControlPlane>,
    method: Method,
    target: &str,
    body: Option<&str>,
) -> (u16, String, String) {
    let service = TowerService::new(router(plane));
    let mut builder = Request::builder().method(method).uri(target);
    if body.is_some() {
        builder = builder.header("content-type", "application/x-www-form-urlencoded");
    }
    let body = body.map_or_else(topcoat::router::Body::empty, |body| {
        topcoat::router::Body::from(body.to_owned())
    });
    let Ok(request) = builder.body(body) else {
        return (
            0,
            String::new(),
            "the request could not be built".to_owned(),
        );
    };
    let response = match tower::ServiceExt::oneshot(service, request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    read(response).await
}

async fn read(response: Response) -> (u16, String, String) {
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    match topcoat::router::to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => (
            status,
            location,
            String::from_utf8_lossy(&bytes).into_owned(),
        ),
        Err(_) => (status, location, String::new()),
    }
}

#[tokio::test]
async fn the_index_renders_the_listing_the_header_and_the_transcript() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Completed));
    let (status, _, body) = send(fake, Method::GET, "/", None).await;

    assert_eq!(status, 200);
    assert!(body.contains("first instruction"), "{body}");
    assert!(body.contains("the agent answered"), "{body}");
    assert!(body.contains("mokmok"), "{body}");
    assert!(body.contains("aria-label=\"Sessions\""), "{body}");
    assert!(body.contains("Completed"), "{body}");
}

#[tokio::test]
async fn a_running_session_renders_a_live_region() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Running));
    let (status, _, body) = send(fake, Method::GET, "/", None).await;

    assert_eq!(status, 200);
    assert!(body.contains("live-note"), "{body}");
    assert!(body.contains("action=\"/sessions/cancel\""), "{body}");
    assert!(
        body.contains("<meta http-equiv=\"refresh\" content=\"2; url="),
        "a running session re-reads itself: {body}"
    );
}

#[tokio::test]
async fn a_finished_session_has_no_live_region_and_no_cancel() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Completed));
    let (_, _, body) = send(fake, Method::GET, "/", None).await;

    assert!(!body.contains("live-note"), "{body}");
    assert!(!body.contains("action=\"/sessions/cancel\""), "{body}");
}

#[tokio::test]
async fn an_interrupted_session_explains_itself_and_stops_refreshing() {
    let mut interrupted = Fake::with_session(TaskStatus::Interrupted);
    interrupted.sessions[0].outcome = Some(INTERRUPTED_REASON.to_owned());
    let fake = Arc::new(interrupted);
    let (status, _, body) = send(fake, Method::GET, "/", None).await;

    assert_eq!(status, 200);
    assert!(body.contains("Interrupted"), "{body}");
    assert!(
        body.contains("badge tone-warning"),
        "the badge carries the warning tone: {body}"
    );
    assert!(body.contains(INTERRUPTED_REASON), "{body}");
    assert!(!body.contains("<meta http-equiv=\"refresh\""), "{body}");
    assert!(!body.contains("action=\"/sessions/cancel\""), "{body}");
}

#[tokio::test]
async fn an_empty_control_plane_renders_the_empty_state() {
    let (status, _, body) = send(Arc::new(Fake::default()), Method::GET, "/", None).await;

    assert_eq!(status, 200);
    assert!(body.contains("No sessions yet"), "{body}");
}

#[tokio::test]
async fn a_failed_listing_renders_a_banner_instead_of_dropping_the_page() {
    let fake = Arc::new(Fake {
        fail: true,
        ..Fake::with_session(TaskStatus::Completed)
    });
    let (status, _, body) = send(fake, Method::GET, "/", None).await;

    assert_eq!(status, 200);
    assert!(body.contains("role=\"alert\""), "{body}");
    assert!(body.contains("unavailable"), "{body}");
}

#[tokio::test]
async fn the_stylesheet_is_served_with_its_content_type() {
    let (status, _, body) = send(Arc::new(Fake::default()), Method::GET, "/app.css", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("--brand"), "{body}");
}

#[tokio::test]
async fn a_submitted_form_answers_a_redirect_to_the_session() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Submitted));
    let (status, location, _) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::POST,
        "/sessions",
        Some("instruction=do+it&agent=local&workspace=%2Fsrv%2Fone&theme=dark"),
    )
    .await;

    assert_eq!(status, 303);
    assert_eq!(
        location,
        format!(
            "/?workspace=/srv/one&theme=dark&session={}",
            fake.id().as_str()
        )
    );
    let submitted = fake.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].instruction, "do it");
    assert_eq!(submitted[0].workspace.as_deref(), Some("/srv/one"));
}

#[tokio::test]
async fn the_composer_posts_the_request_id_it_minted() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Submitted));
    let (_, _, body) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::GET,
        "/",
        None,
    )
    .await;

    let minted = minted(&body).expect("the composer mints a request id");
    assert_eq!(
        uuid::Uuid::parse_str(&minted).map(|id| id.get_version_num()),
        Ok(7),
        "the composer mints a UUIDv7: {minted}"
    );

    let form = format!(
        "instruction=do+it&agent=local&workspace=%2Fsrv%2Fone&request_id={minted}&theme=dark"
    );
    let (status, _, _) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::POST,
        "/sessions",
        Some(&form),
    )
    .await;

    assert_eq!(status, 303);
    assert_eq!(
        fake.submitted()[0].request_id.as_deref(),
        Some(minted.as_str())
    );
}

#[tokio::test]
async fn two_renders_mint_different_request_ids() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Submitted));
    let (_, _, first) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::GET,
        "/",
        None,
    )
    .await;
    let (_, _, second) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::GET,
        "/",
        None,
    )
    .await;

    assert_ne!(
        minted(&first).expect("the composer mints a request id"),
        minted(&second).expect("the composer mints a request id"),
        "a composer that minted once would give every visit one id"
    );
}

#[tokio::test]
async fn an_empty_instruction_is_refused_without_submitting() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Submitted));
    let (status, location, _) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::POST,
        "/sessions",
        Some("instruction=+&agent=local&theme=dark"),
    )
    .await;

    assert_eq!(status, 303);
    assert!(
        location.contains("error=The%20instruction%20is%20empty"),
        "{location}"
    );
    assert_eq!(fake.submitted().len(), 0);
}

#[tokio::test]
async fn a_cancel_posts_to_the_task_and_returns_to_it() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Running));
    let id = fake.id();
    let form = format!("task={}&workspace=%2Fsrv%2Fone&theme=dark", id.as_str());
    let (status, location, _) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::POST,
        "/sessions/cancel",
        Some(&form),
    )
    .await;

    assert_eq!(status, 303);
    assert_eq!(
        location,
        format!("/?workspace=/srv/one&theme=dark&session={}", id.as_str())
    );
    assert_eq!(fake.cancelled(), vec![id.as_str().to_owned()]);
}

#[tokio::test]
async fn a_bad_task_id_is_refused_without_cancelling() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Running));
    let (status, location, _) = send(
        Arc::clone(&fake) as Arc<dyn ControlPlane>,
        Method::POST,
        "/sessions/cancel",
        Some("task=not-a-uuid&theme=dark"),
    )
    .await;

    assert_eq!(status, 303);
    assert!(
        location.contains("error=That%20task%20id%20is%20not%20valid"),
        "{location}"
    );
    assert_eq!(fake.cancelled().len(), 0);
}

#[tokio::test]
async fn the_light_theme_reaches_the_document() {
    let (_, _, body) = send(
        Arc::new(Fake::with_session(TaskStatus::Completed)),
        Method::GET,
        "/?theme=light",
        None,
    )
    .await;

    assert!(body.contains("data-mode=\"light\""), "{body}");
}

#[tokio::test]
async fn a_cross_origin_form_post_is_refused() {
    let fake = Arc::new(Fake::with_session(TaskStatus::Submitted));
    let service = TowerService::new(router(Arc::clone(&fake) as Arc<dyn ControlPlane>));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/sessions")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("origin", "https://evil.example")
        .body(topcoat::router::Body::from(
            "instruction=steal&agent=local".to_owned(),
        ));
    let Ok(request) = request else {
        panic!("the request could not be built");
    };
    let response = match tower::ServiceExt::oneshot(service, request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };

    assert_eq!(response.status().as_u16(), 403);
    assert_eq!(fake.submitted().len(), 0);
}

#[tokio::test]
async fn a_request_without_a_workspace_shows_the_first_group() {
    let (_, _, body) = send(
        Arc::new(Fake::with_session(TaskStatus::Completed)),
        Method::GET,
        "/",
        None,
    )
    .await;

    assert!(body.contains("the agent answered"), "{body}");
    assert!(body.contains("workspace-path\">/srv/one"), "{body}");
}

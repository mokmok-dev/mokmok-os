//! The HTTP API of the control plane.
//!
//! [`build_router`] serves the same JSON API the `mokmok-controld` binary runs, so a
//! test drives the whole surface with `tower::ServiceExt::oneshot` instead of a
//! socket. The browser UI is a separate app under `apps/web`.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use mokmok_control::{
    CloudEvent, ControlError, ControlEvent, ControlPublisher, ControlSystem, EnumerableStore,
    Error as SystemError, NewTask, Runner, Supervisor, TaskId, TaskState, TaskStatus,
    journal_event,
};
use mokmok_journal::{Journal, JournalEntry, JournalError, JournalEvent, JournalTail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;

const MAX_BODY_BYTES: usize = 128 * 1024;
const EVENT_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_CANCEL_REASON: &str = "cancelled from the control plane";

/// How many recent events a detail request returns unless it asks otherwise.
const DETAIL_EVENT_TAIL: usize = 200;

/// How many recent journal entries a journal request returns unless it asks otherwise.
const DEFAULT_JOURNAL_TAIL: usize = 200;

/// The largest journal tail a request may ask for.
const MAX_JOURNAL_TAIL: usize = 1000;

/// Concurrent journal reads the daemon will run.
///
/// One read decodes a task's whole readable journal, so concurrent requests multiply the
/// memory a loopback client or a cross-origin `no-cors` GET can commit. The UI reads the
/// journals of one workspace and follows one live run, so two permits let a live poll
/// proceed while a page opens.
const JOURNAL_READ_PERMITS: usize = 2;

/// Concurrent store reads the daemon will run.
///
/// The same limit covers both backends: a filesystem read is cheap, while an
/// object-storage read is a network round trip, and one read of a listing is one round
/// trip per task. The UI lists tasks every two seconds and reads one running task's
/// detail per poll, so four permits let those overlap without queueing an unbounded
/// number of round trips.
const STORE_READ_PERMITS: usize = 4;

/// The answer to an `events` value outside `tail`, `all`, and `none`.
const EVENTS_QUERY: &str = "the events query must be one of: tail, all, none";

/// The answer to a `limit` outside `1..=1000`.
const JOURNAL_QUERY: &str = "the journal query limit must be an integer between 1 and 1000";

/// Everything the handlers share: the task actors, the workers, and the live feeds.
#[derive(Clone)]
pub struct AppState {
    pub system: ControlSystem,
    pub supervisor: Arc<Supervisor>,
    pub store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
    pub journal: Arc<dyn Journal>,
    pub agents: Vec<String>,
    pub default_agent: String,
    // Names the agent table's `hosts` map accepts as transport overrides.
    pub transport_hosts: Vec<String>,
    // Exact `Host` header values the daemon answers. A rebinding page reaches the socket
    // with its own host name, so this is what keeps it out without a token.
    pub allowed_hosts: Vec<String>,
    pub token: Option<String>,
    pub events: broadcast::Sender<String>,
    pub journal_events: broadcast::Sender<String>,
    // Bounds the reads a client can start at once; see `JOURNAL_READ_PERMITS`.
    pub journal_reads: Arc<Semaphore>,
    // Bounds the store reads a client can start at once; see `STORE_READ_PERMITS`.
    pub store_reads: Arc<Semaphore>,
    // An `Sse` body over this state's own broadcast sender never ends, which is what
    // holds `axum::serve` open forever; flipping this watch is how a shutdown ends it.
    pub shutdown: watch::Sender<bool>,
}

impl AppState {
    // A store read is a filesystem read or an object-storage round trip, so it must run
    // off the worker threads other requests share. The permit sits inside the blocking
    // closure, so a client that disconnects cannot free it while its read still runs.
    async fn store_read<T, F>(
        &self,
        read: F,
    ) -> Result<T, ApiError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.store_reads)
            .acquire_owned()
            .await
            .map_err(|_| ApiError::internal(STORE_FAILED))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read()
        })
        .await
        .map_err(|error| {
            tracing::debug!(%error, "the store read task failed");
            ApiError::internal(STORE_FAILED)
        })
    }
}

/// Every `Host` header value the daemon answers for a bound `address`.
///
/// A same-origin browser sends `Host: <name>:<port>` whenever the port is not the
/// scheme's default, so each loopback spelling appears with and without the port. A
/// bare host name always means "the port this server bound".
#[must_use]
pub fn allowed_hosts(
    address: SocketAddr,
    extra: &[String],
) -> Vec<String> {
    let port = address.port();
    let bare = match address.ip() {
        IpAddr::V4(_) => address.ip().to_string(),
        IpAddr::V6(_) => format!("[{}]", address.ip()),
    };
    let mut hosts = vec![
        "127.0.0.1".to_owned(),
        "localhost".to_owned(),
        "[::1]".to_owned(),
        bare.clone(),
        address.to_string(),
    ];
    for name in ["127.0.0.1", "localhost", "[::1]", bare.as_str()] {
        hosts.push(format!("{name}:{port}"));
    }
    for name in extra {
        hosts.push(name.clone());
        if !has_port(name) {
            hosts.push(format!("{name}:{port}"));
        }
    }
    hosts
}

fn has_port(name: &str) -> bool {
    name.rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok())
}

/// The deployment choices the router serves.
#[derive(Clone, Debug, Default)]
pub struct ControlConfig {
    /// Agent names a task may ask for.
    pub agents: Vec<String>,
    /// The agent an empty name resolves to.
    pub default_agent: String,
    /// Transport-override names the agent table's `hosts` map accepts.
    pub transport_hosts: Vec<String>,
    /// Exact `Host` header values the daemon answers.
    pub allowed_hosts: Vec<String>,
    /// Worker threads that run submitted tasks.
    pub workers: usize,
    /// Bearer token the `/api` routes require, when set.
    pub token: Option<String>,
}

/// Builds the state the router serves.
#[must_use]
pub fn build_state(
    store: Arc<dyn EnumerableStore<TaskId, ControlEvent>>,
    journal: Arc<dyn Journal>,
    runner: Arc<dyn Runner>,
    config: ControlConfig,
) -> AppState {
    let ControlConfig {
        agents,
        default_agent,
        transport_hosts,
        allowed_hosts,
        workers,
        token,
    } = config;
    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let (journal_events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let (shutdown, _) = watch::channel(false);
    let publisher: Arc<dyn ControlPublisher> = Arc::new(BroadcastPublisher {
        events: events.clone(),
    });
    let journal: Arc<dyn Journal> = Arc::new(BroadcastJournal {
        inner: journal,
        events: journal_events.clone(),
    });
    let system = ControlSystem::new(Arc::clone(&store), publisher);
    let supervisor = Arc::new(Supervisor::new(
        system.clone(),
        runner,
        Arc::clone(&journal),
        workers,
    ));
    AppState {
        system,
        supervisor,
        store,
        journal,
        agents,
        default_agent,
        transport_hosts,
        allowed_hosts,
        token,
        events,
        journal_events,
        journal_reads: Arc::new(Semaphore::new(JOURNAL_READ_PERMITS)),
        store_reads: Arc::new(Semaphore::new(STORE_READ_PERMITS)),
        shutdown,
    }
}

/// Builds the router, its host check, its auth middleware, and the body limit over `state`.
pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/agents", get(agents))
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/{id}", get(task_detail))
        .route("/api/tasks/{id}/journal", get(task_journal))
        .route("/api/tasks/{id}/cancel", post(cancel_task))
        .route("/api/events", get(event_stream))
        .route("/api/journal/events", get(journal_stream))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state.clone());

    Router::new()
        .route("/healthz", get(healthz))
        .merge(api)
        .layer(middleware::from_fn_with_state(state, require_allowed_host))
}

struct BroadcastPublisher {
    events: broadcast::Sender<String>,
}

impl ControlPublisher for BroadcastPublisher {
    fn publish(
        &self,
        events: &[CloudEvent<ControlEvent>],
    ) {
        for event in events {
            if let Ok(line) = serde_json::to_string(event) {
                let _ = self.events.send(line);
            }
        }
    }
}

// The live journal feed publishes the stamped entry beside the task, so a browser can
// tell which transcript a line belongs to without another request.
struct BroadcastJournal {
    inner: Arc<dyn Journal>,
    events: broadcast::Sender<String>,
}

impl Journal for BroadcastJournal {
    fn append(
        &self,
        task: &str,
        events: &[JournalEvent],
    ) -> Result<Vec<JournalEntry>, JournalError> {
        let entries = self.inner.append(task, events)?;
        for entry in &entries {
            if let Ok(line) = serde_json::to_string(&json!({"task": task, "entry": entry})) {
                let _ = self.events.send(line);
            }
        }
        Ok(entries)
    }

    fn tail(
        &self,
        task: &str,
        limit: usize,
    ) -> Result<JournalTail, JournalError> {
        self.inner.tail(task, limit)
    }

    fn count(
        &self,
        task: &str,
    ) -> Result<u64, JournalError> {
        self.inner.count(task)
    }
}

async fn healthz() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "ok",
    )
        .into_response()
}

async fn agents(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "agents": state.agents,
        "default": state.default_agent,
        "hosts": state.transport_hosts,
    }))
}

async fn list_tasks(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let system = state.system.clone();
    let journal = Arc::clone(&state.journal);
    let tasks = state
        .store_read(move || {
            let mut tasks = system.list()?;
            for task in &mut tasks {
                // A task logged before the split has no journal, so the domain count stays.
                match journal.count(task.id.as_str()) {
                    Ok(count) if count > 0 => task.updates = count,
                    Ok(_) => {},
                    // A failed count is retried on the next request, so a warning per poll
                    // would bury the log for as long as the cause lasts.
                    Err(error) => {
                        tracing::debug!(
                            task = task.id.as_str(),
                            %error,
                            "cannot count the journal"
                        );
                    },
                }
            }
            Ok(tasks)
        })
        .await?
        .map_err(ApiError::from_system)?;
    Ok(Json(json!({"tasks": tasks})))
}

async fn create_task(
    State(state): State<AppState>,
    Json(new): Json<NewTask>,
) -> Result<Response, ApiError> {
    if !state.agents.iter().any(|agent| agent == &new.agent) {
        return Err(ApiError::bad_request(format!(
            "no agent is named {:?}",
            new.agent
        )));
    }
    let id = state
        .supervisor
        .submit(new)
        .map_err(ApiError::from_system)?;
    tracing::info!(task = id.as_str(), "task submitted");
    Ok((StatusCode::CREATED, Json(json!({"id": id}))).into_response())
}

async fn task_detail(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::bad_request(EVENTS_QUERY))?;
    let id = parse_id(raw)?;
    let store = Arc::clone(&state.store);
    let events = state
        .store_read(move || store.load(&id))
        .await?
        .map_err(|_| ApiError::internal(STORE_FAILED))?;
    let task = TaskState::replay(&events)
        .map_err(|error| ApiError::from_system(SystemError::Control(error)))?;
    if task.spec.is_none() {
        return Err(ApiError::not_found("no such task"));
    }
    let event_count = events.len();
    let selected: Vec<_> = match query.events {
        EventMode::Tail => {
            let start = event_count.saturating_sub(DETAIL_EVENT_TAIL);
            events[start..].to_vec()
        },
        EventMode::All => events,
        EventMode::None => Vec::new(),
    };
    Ok(Json(
        json!({"state": task, "events": selected, "event_count": event_count}),
    ))
}

#[derive(Deserialize)]
struct DetailQuery {
    #[serde(default)]
    events: EventMode,
}

#[derive(Deserialize)]
struct JournalQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn task_journal(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    query: Result<Query<JournalQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::bad_request(JOURNAL_QUERY))?;
    let limit = query.limit.unwrap_or(DEFAULT_JOURNAL_TAIL);
    if !(1..=MAX_JOURNAL_TAIL).contains(&limit) {
        return Err(ApiError::bad_request(JOURNAL_QUERY));
    }
    let id = parse_id(raw)?;
    let store = Arc::clone(&state.store);
    let stream = id.clone();
    let events = state
        .store_read(move || store.load(&stream))
        .await?
        .map_err(|_| ApiError::internal(STORE_FAILED))?;
    let task = TaskState::replay(&events)
        .map_err(|error| ApiError::from_system(SystemError::Control(error)))?;
    if task.spec.is_none() {
        return Err(ApiError::not_found("no such task"));
    }
    // A tail reads and decodes a task's segments, so it must not run on a worker that
    // other requests need, and a permit bounds how many such reads a client can start
    // at once.
    let journal = Arc::clone(&state.journal);
    let permit = Arc::clone(&state.journal_reads)
        .acquire_owned()
        .await
        .map_err(|_| ApiError::internal(JOURNAL_FAILED))?;
    let tail = tokio::task::spawn_blocking(move || {
        // The permit is held by the read, not by this request's frame: a client that
        // disconnects drops the frame while the read keeps running to completion.
        let _permit = permit;
        journal.tail(id.as_str(), limit)
    })
    .await
    .map_err(|error| {
        tracing::debug!(%error, "the journal read task failed");
        ApiError::internal(JOURNAL_FAILED)
    })?
    .map_err(|_| ApiError::internal(JOURNAL_FAILED))?;
    // The journal wins once it has entries. A task logged before the split has none,
    // so its transcript is still in the domain log; reading it out must not write it
    // back, and a task with both shows only its journal.
    if tail.entries.is_empty() {
        return Ok(Json(project_updates(&events, limit)));
    }
    Ok(Json(json!({"entries": tail.entries, "total": tail.total})))
}

fn project_updates(
    events: &[CloudEvent<ControlEvent>],
    limit: usize,
) -> Value {
    let mut entries = Vec::new();
    for event in events {
        let ControlEvent::UpdateRecorded { update } = &event.data else {
            continue;
        };
        entries.push(JournalEntry {
            seq: event.version.get(),
            time: event.time.clone().unwrap_or_default(),
            event: journal_event(update).clipped(),
        });
    }
    let total = entries.len();
    entries.drain(..total.saturating_sub(limit));
    json!({"entries": entries, "total": total})
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum EventMode {
    #[default]
    Tail,
    All,
    None,
}

#[derive(Deserialize)]
struct CancelBody {
    #[serde(default)]
    reason: Option<String>,
}

async fn cancel_task(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Json(body): Json<CancelBody>,
) -> Result<Response, ApiError> {
    let id = parse_id(raw)?;
    let system = state.system.clone();
    let stream = id.clone();
    let task = state
        .store_read(move || system.state(&stream))
        .await?
        .map_err(ApiError::from_system)?;
    if task.spec.is_none() {
        return Err(ApiError::not_found("no such task"));
    }
    if matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        return Err(ApiError::conflict("the task has already finished"));
    }
    let reason = body
        .reason
        .unwrap_or_else(|| DEFAULT_CANCEL_REASON.to_owned());
    state
        .supervisor
        .cancel(&id, &reason)
        .map_err(ApiError::from_system)?;
    tracing::info!(task = id.as_str(), "task cancellation requested");
    Ok((StatusCode::ACCEPTED, Json(json!({"id": id}))).into_response())
}

async fn event_stream(
    State(state): State<AppState>
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    stream_lines(state.events.subscribe(), state.shutdown.subscribe())
}

async fn journal_stream(
    State(state): State<AppState>
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    stream_lines(state.journal_events.subscribe(), state.shutdown.subscribe())
}

fn stream_lines(
    receiver: broadcast::Receiver<String>,
    mut shutdown: watch::Receiver<bool>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(receiver)
        .take_until(async move {
            let _ = shutdown.changed().await;
        })
        .filter_map(
            |message| async move { message.ok().map(|line| Ok(Event::default().data(line))) },
        );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn parse_id(raw: String) -> Result<TaskId, ApiError> {
    TaskId::parse(raw).map_err(|error| ApiError::from_system(SystemError::Control(error)))
}

// DNS rebinding reaches the loopback socket with the attacker's host name still in the
// `Host` header, so the allowlist is checked before anything else is served.
async fn require_allowed_host(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if state
        .allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host))
    {
        next.run(request).await
    } else {
        ApiError::forbidden("the Host header is not allowed").into_response()
    }
}

// The token never travels in a URL: this middleware reads the header, so no proxy log
// and no browser history can hold it.
async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_deref() else {
        return next.run(request).await;
    };
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token);
    match provided {
        Some(provided) if constant_time_eq(expected.as_bytes(), provided.as_bytes()) => {
            next.run(request).await
        },
        _ => {
            let mut response = ApiError::unauthorized("a bearer token is required").into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"mokmok\""),
            );
            response
        },
    }
}

// RFC 7235 makes the scheme case-insensitive; only the token itself is compared in
// constant time.
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    Some(token.trim_start_matches(' '))
}

// Compares every byte instead of stopping at the first difference, so the time the
// answer takes does not reveal how much of a guessed token was right.
//
// The length check does reveal the token's length, which is inherent to a
// variable-length credential and accepted here.
fn constant_time_eq(
    expected: &[u8],
    provided: &[u8],
) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(provided) {
        difference |= left ^ right;
    }
    difference == 0
}

const STORE_FAILED: &str = "the event store failed";
const JOURNAL_FAILED: &str = "the journal could not be read";

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    fn new(
        status: StatusCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    // No store message reaches a client: a StoreError carries a filesystem path.
    fn from_system(error: SystemError) -> Self {
        match error {
            SystemError::Control(ControlError::InvalidSpec { field, reason }) => {
                Self::bad_request(format!("{field} {reason}"))
            },
            SystemError::Control(ControlError::InvalidTaskId(_)) => {
                Self::bad_request("the task id is not a canonical UUID")
            },
            SystemError::Control(ControlError::NotSubmitted) => Self::not_found("no such task"),
            SystemError::Control(ControlError::Terminal) => {
                Self::conflict("the task has already finished")
            },
            SystemError::Store(_) => Self::internal(STORE_FAILED),
            SystemError::Control(_) | SystemError::ActorUnavailable | SystemError::Spawn(_) => {
                Self::internal("the control plane could not serve the request")
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allows(
        hosts: &[String],
        host: &str,
    ) -> bool {
        hosts.iter().any(|allowed| allowed == host)
    }

    #[test]
    fn a_loopback_bind_allows_each_name_with_and_without_the_port() {
        let address: SocketAddr = "127.0.0.1:18787".parse().expect("an address");
        let hosts = allowed_hosts(address, &[]);
        for host in [
            "127.0.0.1",
            "127.0.0.1:18787",
            "localhost",
            "localhost:18787",
            "[::1]",
            "[::1]:18787",
        ] {
            assert!(allows(&hosts, host), "{host} is missing from {hosts:?}");
        }
    }

    #[test]
    fn a_wildcard_bind_allows_its_bare_address_and_the_port_form() {
        let address: SocketAddr = "0.0.0.0:18788".parse().expect("an address");
        let hosts = allowed_hosts(address, &[]);
        for host in ["0.0.0.0", "0.0.0.0:18788", "127.0.0.1:18788"] {
            assert!(allows(&hosts, host), "{host} is missing from {hosts:?}");
        }
    }

    #[test]
    fn an_extra_name_gets_the_port_form_and_is_not_doubled() {
        let address: SocketAddr = "127.0.0.1:18787".parse().expect("an address");
        let hosts = allowed_hosts(
            address,
            &[
                "control.example".to_owned(),
                "already.example:9000".to_owned(),
                "[2001:db8::1]".to_owned(),
            ],
        );
        assert!(allows(&hosts, "control.example"));
        assert!(allows(&hosts, "control.example:18787"));
        assert!(allows(&hosts, "already.example:9000"));
        assert!(!allows(&hosts, "already.example:9000:18787"));
        assert!(allows(&hosts, "[2001:db8::1]:18787"));
    }

    #[test]
    fn an_ipv6_bind_is_bracketed() {
        let address: SocketAddr = "[::1]:18787".parse().expect("an address");
        let hosts = allowed_hosts(address, &[]);
        assert!(allows(&hosts, "[::1]"));
        assert!(allows(&hosts, "[::1]:18787"));
        assert!(
            !allows(&hosts, "::1:18787"),
            "an unbracketed v6 host is ambiguous"
        );
    }
}

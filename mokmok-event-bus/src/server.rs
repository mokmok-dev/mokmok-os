use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::SignalKind;
use tokio::sync::watch;

use crate::bus::{Bus, ConnectionPermit, OpenError, TopicEntry};
use crate::config::{Config, Limits, Timeouts};
use crate::event::{Cursor, Lapsed, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::store::StoreError;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;
use crate::wire::{self, Request, SubId};

const GOING_AWAY: u16 = 1001;
const PROTOCOL_ERROR: u16 = 1002;
const POLICY_VIOLATION: u16 = 1008;
const INTERNAL_ERROR: u16 = 1011;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("another process is already serving this socket")]
    AlreadyRunning,
    #[error("the event database could not be served")]
    Store(#[source] StoreError),
    #[error("the socket could not be served: {0}")]
    Io(#[source] io::Error),
}

/// Serves the bus on `config.path` until `shutdown` resolves, then unlinks the socket.
///
/// # Errors
///
/// Returns [`ServeError::AlreadyRunning`] when a live peer answers on the socket,
/// [`ServeError::Store`] when the database cannot be opened, and [`ServeError::Io`] when the
/// socket cannot be prepared or served.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let (socket, listener) = SocketFile::bind(&config.path).await?;
    let bus = Bus::open(config.clone())?;
    let router = Router::new().route("/ws", get(upgrade)).with_state(Server {
        bus: Arc::clone(&bus),
        config,
    });

    let stop = bus.stop_handle();
    let graceful = async move {
        shutdown.await;
        // Flipped before axum starts waiting, or a session parked on a pull would never see it.
        stop.send_replace(true);
    };

    let served = axum::serve(listener, router)
        .with_graceful_shutdown(graceful)
        .await;

    drop(socket);
    let stopped = bus.shutdown().await;
    served.map_err(ServeError::Io)?;
    stopped.map_err(ServeError::Store)
}

/// Resolves on SIGINT or SIGTERM.
pub async fn signals() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            },
            // A process that cannot be told to stop should keep serving rather than exit.
            Err(_) => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}

#[derive(Clone)]
struct Server {
    bus: Arc<Bus>,
    config: Config,
}

struct SocketFile {
    path: PathBuf,
}

impl SocketFile {
    /// Probes before binding: a socket nobody answers is a stale file left by a crash, and the
    /// only safe way to tell it from a live server is to connect to it.
    async fn bind(path: &Path) -> Result<(Self, UnixListener), ServeError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let created = !parent.exists();
            std::fs::create_dir_all(parent).map_err(ServeError::Io)?;

            if created {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(ServeError::Io)?;
            }
        }

        match UnixStream::connect(path).await {
            Ok(_) => return Err(ServeError::AlreadyRunning),
            Err(error) if is_stale(&error) => {},
            Err(error) => return Err(ServeError::Io(error)),
        }

        match std::fs::remove_file(path) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(ServeError::Io(error)),
        }

        let listener = UnixListener::bind(path).map_err(ServeError::Io)?;
        let socket = Self {
            path: path.to_path_buf(),
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(ServeError::Io)?;

        Ok((socket, listener))
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_stale(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

async fn upgrade(
    State(server): State<Server>,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Enforced as the frame is assembled: checking a complete `Message::Text` would let a peer
    // make the always-on process allocate a frame of any size before the check ran.
    let frame_bytes = server.config.limits.frame_bytes.get();

    let Some(permit) = server.bus.try_connect() else {
        tracing::warn!("refused a connection at the ceiling");
        return (StatusCode::SERVICE_UNAVAILABLE, "connection_limit").into_response();
    };

    if *server.bus.stop_receiver().borrow_and_update() {
        drop(permit);
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting_down").into_response();
    }

    upgrade
        .max_message_size(frame_bytes)
        .on_upgrade(move |socket| session(socket, server, permit))
}

struct Reader {
    topic: Topic,
    entry: Arc<TopicEntry>,
    subscriber: SubscriberId,
    /// The next seq this session may hand out.
    delivered: Seq,
    /// The checkpoint, cached; the database is the truth.
    acked: Seq,
    ticks: watch::Receiver<Window>,
    parked: Option<Parked>,
}

/// Why a pull that found nothing is waiting. Conflating the two either spins the bus on a commit
/// that cannot help or floods a consumer whose credit is spent.
enum Parked {
    /// Caught up: the next event will come from a commit, so wait on the topic's watch.
    CaughtUp { req_id: u64, max: NonZeroUsize },
    /// The credit is spent: only an `Ack` can free more, so a commit must not wake this.
    Spent { req_id: u64, max: NonZeroUsize },
}

/// The result of handling one request: zero or more responses to send, or a close code.
enum Outcome {
    Answers(Vec<wire::Response>),
    Quiet,
    Close(u16),
}

struct Session {
    subscriptions: HashMap<SubId, Reader>,
    next_sub: u64,
    pinged: Option<tokio::time::Instant>,
}

async fn session(
    mut socket: WebSocket,
    server: Server,
    _permit: ConnectionPermit,
) {
    tracing::debug!(connections = server.bus.connections(), "session opened");

    let timeouts = server.config.timeouts;
    let limits = server.config.limits;
    let mut stop = server.bus.stop_receiver();
    let mut heartbeat = tokio::time::interval(millis(timeouts.heartbeat));
    let mut session = Session {
        subscriptions: HashMap::new(),
        next_sub: 0,
        pinged: None,
    };

    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { return };

                if !deliver(&mut socket, &server, &limits, &mut session, frame, timeouts).await {
                    return;
                }
            }
            _ = heartbeat.tick() => {
                if !beat(&mut socket, &mut session, timeouts).await {
                    return;
                }
            }
            () = any_window_changed(&mut session.subscriptions) => {
                if !recheck(&mut socket, &limits, &mut session, timeouts).await {
                    return;
                }
            }
            _ = stop.changed() => {
                close(&mut socket, GOING_AWAY, "shutting down", timeouts).await;
                return;
            }
        }
    }
}

/// Handles one received frame; `false` means the session ends here.
async fn deliver(
    socket: &mut WebSocket,
    server: &Server,
    limits: &Limits,
    session: &mut Session,
    frame: Message,
    timeouts: Timeouts,
) -> bool {
    match frame {
        Message::Text(text) => {
            let Some(request) = wire::RequestFrame::decode(&text) else {
                close(socket, PROTOCOL_ERROR, "not a request frame", timeouts).await;
                return false;
            };

            apply(socket, server, limits, session, request, timeouts).await
        },
        Message::Pong(_) => {
            session.pinged = None;
            true
        },
        Message::Close(_) => false,
        // A ping needs no answer here: tungstenite replies while reading, and the bus speaks
        // nothing but text.
        Message::Ping(_) | Message::Binary(_) => true,
    }
}

async fn apply(
    socket: &mut WebSocket,
    server: &Server,
    limits: &Limits,
    session: &mut Session,
    request: Request,
    timeouts: Timeouts,
) -> bool {
    let outcome = handle(server, limits, session, request).await;

    apply_outcome(socket, outcome, timeouts).await
}

async fn handle(
    server: &Server,
    limits: &Limits,
    session: &mut Session,
    request: Request,
) -> Outcome {
    match request {
        Request::Publish {
            req_id,
            topic,
            payload,
        } => publish(server, req_id, topic, payload).await,
        Request::Subscribe {
            req_id,
            topic,
            subscriber,
        } => subscribe(server, session, req_id, topic, subscriber).await,
        Request::Pull { req_id, sub, max } => pull(limits, session, req_id, sub, max).await,
        Request::Ack {
            req_id,
            sub,
            through,
        } => ack(server, limits, session, req_id, sub, through).await,
    }
}

async fn publish(
    server: &Server,
    req_id: u64,
    topic: Topic,
    payload: Payload,
) -> Outcome {
    // Held until the append resolves: a topic with no receiver is a reclaim candidate, and a
    // swept entry's watch would not wake the reader that attached after the sweep.
    let (entry, _ticks) = match server.bus.open_topic(topic).await {
        Ok(opened) => opened,
        Err(OpenError::Refused(reason)) => {
            tracing::warn!(%topic, %reason, "refused a topic");
            return Outcome::Answers(vec![wire::Response::Refused { req_id, reason }]);
        },
        Err(OpenError::Store(error)) => {
            tracing::error!(%error, "the store could not open the topic");
            return Outcome::Close(INTERNAL_ERROR);
        },
    };

    match entry.append(topic, payload).await {
        Ok(published) => Outcome::Answers(vec![wire::Response::Published {
            req_id,
            id: published.id(),
            seq: published.seq(),
            at: published.at(),
        }]),
        Err(error) => {
            tracing::error!(%error, "the append failed");
            Outcome::Close(INTERNAL_ERROR)
        },
    }
}

async fn subscribe(
    server: &Server,
    session: &mut Session,
    req_id: u64,
    topic: Topic,
    subscriber: SubscriberId,
) -> Outcome {
    let (entry, ticks) = match server.bus.open_topic(topic).await {
        Ok(opened) => opened,
        Err(OpenError::Refused(reason)) => {
            tracing::warn!(%topic, %reason, "refused a topic");
            return Outcome::Answers(vec![wire::Response::Refused { req_id, reason }]);
        },
        Err(OpenError::Store(error)) => {
            tracing::error!(%error, "the store could not open the topic");
            return Outcome::Close(INTERNAL_ERROR);
        },
    };

    let window = *ticks.borrow();
    let acked = match server.bus.store().checkpoint(subscriber, topic).await {
        Ok(None) => window.first(),
        Ok(Some(cursor)) if cursor.epoch() == window.epoch() => cursor.seq(),
        Ok(Some(_)) => {
            tracing::warn!(%topic, "refused a subscriber from another epoch");
            return Outcome::Answers(vec![wire::Response::Refused {
                req_id,
                reason: Refusal::Unresumable { window },
            }]);
        },
        Err(error) => {
            tracing::error!(%error, "the checkpoint could not be read");
            return Outcome::Close(INTERNAL_ERROR);
        },
    };

    let sub = next_sub(session);
    let reader = Reader {
        topic,
        entry,
        subscriber,
        delivered: acked,
        acked,
        ticks,
        parked: None,
    };
    session.subscriptions.insert(sub, reader);

    Outcome::Answers(vec![wire::Response::Subscribed {
        req_id,
        sub,
        window,
    }])
}

const fn next_sub(session: &mut Session) -> SubId {
    let sub = SubId::new(session.next_sub);
    session.next_sub += 1;
    sub
}

async fn pull(
    limits: &Limits,
    session: &mut Session,
    req_id: u64,
    sub: SubId,
    max: NonZeroUsize,
) -> Outcome {
    let Some(reader) = session.subscriptions.get_mut(&sub) else {
        return Outcome::Answers(vec![wire::Response::Refused {
            req_id,
            reason: Refusal::UnknownSubscription,
        }]);
    };

    if reader.parked.is_some() {
        return Outcome::Answers(vec![wire::Response::Refused {
            req_id,
            reason: Refusal::PullInFlight,
        }]);
    }

    pull_reader(reader, limits, req_id, sub, max).await
}

async fn pull_reader(
    reader: &mut Reader,
    limits: &Limits,
    req_id: u64,
    sub: SubId,
    max: NonZeroUsize,
) -> Outcome {
    reader.ticks.borrow_and_update();
    let window = *reader.ticks.borrow();
    let delivered = reader.delivered;
    let acked = reader.acked;

    let max = max.min(limits.batch);
    let from = Seq::new(delivered.get().max(window.first().get()));
    let ceiling = acked.get().saturating_add(limits.in_flight.get() as u64);
    let credit = usize::try_from(ceiling.saturating_sub(from.get())).unwrap_or(usize::MAX);
    let count = max.get().min(credit);

    if count == 0 {
        if delivered < window.first() {
            reader.delivered = window.first();
            return Outcome::Answers(vec![wire::Response::Batch {
                req_id,
                sub,
                window,
                lapsed: Some(Lapsed::new(
                    Cursor::new(window.epoch(), delivered),
                    window.first(),
                )),
                events: Vec::new(),
            }]);
        }

        reader.parked = Some(Parked::Spent { req_id, max });
        return Outcome::Quiet;
    }

    let count = NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN);

    match reader.entry.store().read(reader.topic, from, count).await {
        Ok((state, events)) if !events.is_empty() => {
            let next = events
                .last()
                .map_or(from, |event| Seq::new(event.seq().get() + 1));
            let window = Window::new(state.epoch, state.first, state.next);
            let lapsed = (delivered < state.first)
                .then(|| Lapsed::new(Cursor::new(state.epoch, delivered), state.first));
            reader.delivered = next;

            Outcome::Answers(vec![wire::Response::Batch {
                req_id,
                sub,
                window,
                lapsed,
                events,
            }])
        },
        Ok(_) => {
            reader.parked = Some(Parked::CaughtUp { req_id, max });
            Outcome::Quiet
        },
        Err(error) => {
            tracing::error!(%error, "the read failed");
            Outcome::Close(INTERNAL_ERROR)
        },
    }
}

async fn ack(
    server: &Server,
    limits: &Limits,
    session: &mut Session,
    req_id: u64,
    sub: SubId,
    through: Cursor,
) -> Outcome {
    let Some(reader) = session.subscriptions.get_mut(&sub) else {
        return Outcome::Answers(vec![wire::Response::Refused {
            req_id,
            reason: Refusal::UnknownSubscription,
        }]);
    };

    let window = reader.entry.window();

    if through.epoch() != window.epoch() {
        return Outcome::Answers(vec![wire::Response::Acked {
            req_id,
            sub,
            through: Cursor::new(window.epoch(), reader.acked),
        }]);
    }

    let clamped = through
        .seq()
        .get()
        .min(reader.delivered.get())
        .max(reader.acked.get());
    let through = Cursor::new(window.epoch(), Seq::new(clamped));

    let stored = match server
        .bus
        .store()
        .advance(reader.subscriber, reader.topic, through)
        .await
    {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!(%error, "the checkpoint could not be advanced");
            return Outcome::Close(INTERNAL_ERROR);
        },
    };
    reader.acked = stored.seq();

    let mut responses = vec![wire::Response::Acked {
        req_id,
        sub,
        through: stored,
    }];

    if let Some(Parked::Spent {
        req_id: parked_id,
        max,
    }) = reader.parked.take()
    {
        let freed = pull_reader(reader, limits, parked_id, sub, max).await;
        match freed {
            Outcome::Answers(mut more) => responses.append(&mut more),
            Outcome::Quiet => {},
            Outcome::Close(code) => return Outcome::Close(code),
        }
    }

    Outcome::Answers(responses)
}

/// Re-polls every reader that was parked caught-up, because one of their watches changed.
async fn recheck(
    socket: &mut WebSocket,
    limits: &Limits,
    session: &mut Session,
    timeouts: Timeouts,
) -> bool {
    let caught_up: Vec<(SubId, u64, NonZeroUsize)> = session
        .subscriptions
        .iter()
        .filter_map(|(sub, reader)| match reader.parked {
            Some(Parked::CaughtUp { req_id, max }) => Some((*sub, req_id, max)),
            _ => None,
        })
        .collect();

    for (sub, req_id, max) in caught_up {
        let Some(reader) = session.subscriptions.get_mut(&sub) else {
            continue;
        };
        reader.parked = None;

        let outcome = pull_reader(reader, limits, req_id, sub, max).await;
        if !apply_outcome(socket, outcome, timeouts).await {
            return false;
        }
    }

    true
}

/// Applies an outcome that arrived outside a normal request frame.
async fn apply_outcome(
    socket: &mut WebSocket,
    outcome: Outcome,
    timeouts: Timeouts,
) -> bool {
    match outcome {
        Outcome::Answers(responses) => {
            for response in responses {
                if !send(socket, &response, timeouts).await {
                    return false;
                }
            }
            true
        },
        Outcome::Quiet => true,
        Outcome::Close(code) => {
            close(socket, code, "frame out of turn", timeouts).await;
            false
        },
    }
}

/// Waits until any parked caught-up reader's window changes.
async fn any_window_changed(subs: &mut HashMap<SubId, Reader>) {
    let waits: Vec<std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>>> = subs
        .values_mut()
        .filter(|reader| matches!(reader.parked, Some(Parked::CaughtUp { .. })))
        .map(|reader| {
            let wait: std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> = Box::pin(async {
                let _ = reader.ticks.changed().await;
            });
            wait
        })
        .collect();

    if waits.is_empty() {
        std::future::pending::<()>().await;
    } else {
        let _ = futures_util::future::select_all(waits).await;
    }
}

/// `false` means the peer stopped answering and the session ends.
async fn beat(
    socket: &mut WebSocket,
    session: &mut Session,
    timeouts: Timeouts,
) -> bool {
    if session
        .pinged
        .is_some_and(|sent| sent.elapsed() >= millis(timeouts.deadline))
    {
        tracing::warn!("closed a peer that stopped answering pings");
        close(socket, POLICY_VIOLATION, "no answer to a ping", timeouts).await;
        return false;
    }

    if session.pinged.is_none() {
        if !write(socket, Message::Ping(Bytes::new()), timeouts).await {
            return false;
        }

        session.pinged = Some(tokio::time::Instant::now());
    }

    true
}

async fn send(
    socket: &mut WebSocket,
    response: &wire::Response,
    timeouts: Timeouts,
) -> bool {
    let Ok(text) = serde_json::to_string(response) else {
        tracing::error!("a response could not be encoded");
        return false;
    };

    write(socket, Message::Text(text.into()), timeouts).await
}

/// A write that will not drain within the deadline closes the socket; a peer that stops reading
/// must not be able to hold the process open.
async fn write(
    socket: &mut WebSocket,
    message: Message,
    timeouts: Timeouts,
) -> bool {
    match tokio::time::timeout(millis(timeouts.deadline), socket.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::debug!(%error, "a write failed");
            false
        },
        Err(_) => {
            tracing::warn!("a write did not drain within the deadline");
            false
        },
    }
}

async fn close(
    socket: &mut WebSocket,
    code: u16,
    reason: &'static str,
    timeouts: Timeouts,
) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };

    write(socket, Message::Close(Some(frame)), timeouts).await;
}

fn millis(value: NonZeroU32) -> Duration {
    Duration::from_millis(u64::from(value.get()))
}

#[cfg(test)]
mod tests {
    use super::{
        Outcome, Parked, Reader, ServeError, Server, Session, SocketFile, is_stale, next_sub, pull,
        signals,
    };
    use crate::bus::Bus;
    use crate::config::{Config, Limits, Retention};
    use crate::event::{Cursor, Seq};
    use crate::payload::Payload;
    use crate::subscriber::SubscriberId;
    use crate::topic::Topic;
    use crate::wire::Response;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::io;
    use std::num::NonZeroUsize;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn scratch(name: &str) -> PathBuf {
        let unique = format!(
            "mokmok-bind-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );

        std::env::temp_dir().join(unique)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn retention(
        events: usize,
        bytes: usize,
    ) -> Retention {
        Retention {
            events: NonZeroUsize::new(events).unwrap(),
            bytes: NonZeroUsize::new(bytes).unwrap(),
            idle: Duration::from_secs(600),
        }
    }

    /// A payload whose raw JSON encoding is exactly `bytes` long; a JSON string is its text plus
    /// the two quotes around it.
    fn payload(bytes: usize) -> Payload {
        Payload::json(&"x".repeat(bytes - 2)).unwrap()
    }

    #[test]
    fn a_missing_or_refused_socket_is_stale() {
        assert!(is_stale(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(is_stale(&io::Error::from(io::ErrorKind::ConnectionRefused)));
    }

    #[test]
    fn a_denied_socket_is_not_stale() {
        assert!(!is_stale(&io::Error::from(io::ErrorKind::PermissionDenied)));
    }

    #[tokio::test]
    async fn serving_creates_a_missing_parent_directory_private() {
        let parent = scratch("private");
        let path = parent.join("bus.sock");
        assert!(!parent.exists(), "the parent starts missing");

        let (socket, _listener) = SocketFile::bind(&path).await.unwrap();

        assert!(parent.is_dir(), "the parent was created");
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the parent this process created stays private");

        drop(socket);
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[tokio::test]
    async fn a_directory_at_the_socket_path_is_reported() {
        let path = scratch("directory");
        std::fs::create_dir_all(&path).unwrap();

        let error = match SocketFile::bind(&path).await {
            Err(ServeError::Io(error)) => error,
            Ok(_) => panic!("a directory was accepted as a socket path"),
            Err(other) => panic!("expected an io error for a directory, got {other:?}"),
        };

        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[tokio::test]
    async fn a_symlink_loop_at_the_socket_path_is_reported() {
        let directory = scratch("loop");
        std::fs::create_dir_all(&directory).unwrap();
        let first = directory.join("first");
        let second = directory.join("second");
        std::os::unix::fs::symlink(&second, &first).unwrap();
        std::os::unix::fs::symlink(&first, &second).unwrap();

        let error = match SocketFile::bind(&first).await {
            Err(ServeError::Io(error)) => error,
            Ok(_) => panic!("a symlink loop was accepted as a socket path"),
            Err(other) => panic!("expected an io error for a symlink loop, got {other:?}"),
        };

        // `ErrorKind::FilesystemLoop` is still unstable on the pinned toolchain, so the kind is
        // compared by name rather than by variant.
        assert_eq!(format!("{:?}", error.kind()), "FilesystemLoop");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[tokio::test]
    async fn signals_stays_pending_until_a_signal_arrives() {
        let resolved = tokio::time::timeout(Duration::from_millis(50), signals()).await;

        assert!(
            resolved.is_err(),
            "the shutdown future resolved with no signal",
        );
    }

    #[tokio::test]
    async fn a_window_wait_with_no_parked_reader_stays_pending() {
        let mut subscriptions = HashMap::new();
        let resolved = tokio::time::timeout(
            Duration::from_millis(50),
            super::any_window_changed(&mut subscriptions),
        )
        .await;

        assert!(
            resolved.is_err(),
            "a wait with nothing parked resolved immediately",
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn a_pull_keeps_the_credit_and_lapse_contract(
            events in 1_usize..=6,
            bytes in 4_usize..=24,
            sizes in prop::collection::vec(2_usize..=6, 0..=8),
            acked in 0_u64..=12,
            delivered in 0_u64..=12,
            in_flight in 1_usize..=6,
            max in 1_usize..=6,
            reclaim in any::<bool>(),
        ) {
            runtime().block_on(async {
                let config = Config {
                    store: PathBuf::from(":memory:"),
                    limits: Limits {
                        in_flight: NonZeroUsize::new(in_flight).unwrap(),
                        ..Limits::default()
                    },
                    retention: retention(events, bytes),
                    ..Config::default()
                };
                let bus = Bus::open(config.clone()).unwrap();
                bus.store().open_topic(Topic::Global).await.unwrap();
                for size in &sizes {
                    bus.store()
                        .append(Topic::Global, payload(*size), SystemTime::now())
                        .await
                        .unwrap();
                }
                if reclaim {
                    bus.store()
                        .reclaim(Topic::Global, SystemTime::now())
                        .await
                        .unwrap();
                }
                let Ok((entry, ticks)) = bus.open_topic(Topic::Global).await else {
                    panic!("the topic could not be opened");
                };
                let server = Server {
                    bus: Arc::clone(&bus),
                    config,
                };
                let subscriber = SubscriberId::new();
                let max = NonZeroUsize::new(max).unwrap();

                let mut session = Session {
                    subscriptions: HashMap::new(),
                    next_sub: 0,
                    pinged: None,
                };
                let sub = next_sub(&mut session);
                let delivered = Seq::new(delivered);
                let acked = Seq::new(acked);
                session.subscriptions.insert(sub, Reader {
                    topic: Topic::Global,
                    entry: Arc::clone(&entry),
                    subscriber,
                    delivered,
                    acked,
                    ticks,
                    parked: None,
                });

                let outcome = pull(
                    &server.config.limits,
                    &mut session,
                    1,
                    sub,
                    max,
                ).await;

                match outcome {
                    Outcome::Answers(responses) => {
                        let Response::Batch { window, lapsed, events, .. } = &responses[0] else {
                            return Err(TestCaseError::fail("pull answered something other than a batch"));
                        };
                        prop_assert!(events.len() <= max.get());
                        for pair in events.windows(2) {
                            prop_assert_eq!(pair[1].seq().get(), pair[0].seq().get() + 1);
                        }

                        let first = window.first().get();
                        let from = delivered.get().max(first);
                        prop_assert_eq!(lapsed.is_some(), delivered.get() < first);
                        if let Some(lapsed) = lapsed {
                            prop_assert_eq!(
                                lapsed.held(),
                                Cursor::new(window.epoch(), Seq::new(delivered.get())),
                            );
                            prop_assert_eq!(lapsed.from(), window.first());
                        }

                        let reader = session.subscriptions.get(&sub).unwrap();
                        if events.is_empty() {
                            prop_assert!(lapsed.is_some(), "an empty batch carried no lapse");
                            prop_assert_eq!(reader.delivered.get(), first);
                        } else {
                            prop_assert_eq!(events.first().unwrap().seq().get(), from);
                            let last = events.last().unwrap().seq().get();
                            prop_assert_eq!(reader.delivered.get(), last + 1);
                            prop_assert!(reader.delivered.get() <= acked.get() + in_flight as u64, "a batch ran past the credit");
                        }
                    },
                    Outcome::Quiet => {
                        let window = entry.window();
                        let from = delivered.get().max(window.first().get());
                        let reader = session.subscriptions.get(&sub).unwrap();
                        prop_assert!(
                            from >= window.next().get() || delivered.get() >= acked.get() + in_flight as u64,
                            "pull parked although the read had events and credit remained",
                        );
                        let spent = matches!(reader.parked, Some(Parked::Spent { .. }));
                        prop_assert_eq!(spent, delivered.get() >= acked.get() + in_flight as u64);
                    },
                    Outcome::Close(code) => {
                        return Err(TestCaseError::fail(format!(
                            "pull closed the session with {code}",
                        )));
                    },
                }

                bus.shutdown().await.unwrap();
                Ok::<(), TestCaseError>(())
            }).unwrap();
        }
    }
}

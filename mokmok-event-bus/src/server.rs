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
use axum::response::Response;
use axum::routing::get;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::SignalKind;
use tokio::sync::watch;

use crate::bus::{Bus, OpenError, TopicEntry};
use crate::config::{Config, Timeouts};
use crate::event::{Cursor, Lapsed, Refusal, Seq, Window};
use crate::payload::Payload;
use crate::store::StoreError;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;
use crate::wire::{self, Request, RequestFrame};

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

    upgrade
        .max_message_size(frame_bytes)
        .on_upgrade(move |socket| session(socket, server))
}

enum Phase {
    Unopened,
    Publisher {
        topic: Topic,
        entry: Arc<TopicEntry>,
    },
    Subscriber {
        topic: Topic,
        entry: Arc<TopicEntry>,
        subscriber: SubscriberId,
        /// The next seq this session may hand out.
        delivered: Seq,
        /// The checkpoint, cached; the database is the truth.
        acked: Seq,
    },
}

/// Why a pull that found nothing is waiting. Conflating the two either spins the bus on a commit
/// that cannot help or floods a consumer whose credit is spent.
enum Parked {
    /// Caught up: the next event will come from a commit, so wait on the topic's watch.
    CaughtUp { max: NonZeroUsize },
    /// The credit is spent: only an `Ack` can free more, so a commit must not wake this.
    Spent,
}

/// Carries the values a frame changed back out of the handler, because a `select!` arm holds its
/// borrows while its handler runs.
enum Step {
    Answer {
        response: wire::Response,
        phase: Phase,
    },
    Park {
        phase: Phase,
        parked: Parked,
    },
    Watch {
        response: wire::Response,
        phase: Phase,
        ticks: watch::Receiver<Window>,
    },
    Close(u16),
}

struct Session {
    phase: Phase,
    ticks: Option<watch::Receiver<Window>>,
    parked: Option<Parked>,
    pinged: Option<tokio::time::Instant>,
}

async fn session(
    mut socket: WebSocket,
    server: Server,
) {
    let Some(_permit) = server.bus.try_connect() else {
        tracing::warn!("refused a connection at the ceiling");
        let refused = wire::Response::Refused {
            reason: Refusal::ConnectionLimit,
        };
        send(&mut socket, &refused, server.config.timeouts).await;
        close(
            &mut socket,
            GOING_AWAY,
            "no room for another connection",
            server.config.timeouts,
        )
        .await;
        return;
    };

    tracing::debug!(connections = server.bus.connections(), "session opened");

    let timeouts = server.config.timeouts;
    let mut stop = server.bus.stop_receiver();
    let mut heartbeat = tokio::time::interval(millis(timeouts.heartbeat));
    let mut session = Session {
        phase: Phase::Unopened,
        ticks: None,
        parked: None,
        pinged: None,
    };

    if *stop.borrow_and_update() {
        close(&mut socket, GOING_AWAY, "shutting down", timeouts).await;
        return;
    }

    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { return };

                if !deliver(&mut socket, &server, &mut session, frame, timeouts).await {
                    return;
                }
            }
            _ = heartbeat.tick() => {
                if !beat(&mut socket, &mut session, timeouts).await {
                    return;
                }
            }
            () = window_changed(&mut session.ticks), if matches!(session.parked, Some(Parked::CaughtUp { .. })) => {
                if !recheck(&mut socket, &server, &mut session, timeouts).await {
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
    session: &mut Session,
    frame: Message,
    timeouts: Timeouts,
) -> bool {
    match frame {
        Message::Text(text) => {
            let Some(request) = RequestFrame::decode(&text) else {
                close(socket, PROTOCOL_ERROR, "not a request frame", timeouts).await;

                return false;
            };

            // Snapshot before the log is read, so an append landing during the read is seen by
            // the log rather than lost behind the park that follows an empty batch.
            if let Some(watching) = session.ticks.as_mut() {
                watching.borrow_and_update();
            }

            apply(socket, server, session, request, timeouts).await
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
    session: &mut Session,
    request: Request,
    timeouts: Timeouts,
) -> bool {
    match handle(server, &session.phase, request).await {
        Step::Answer { response, phase } => {
            session.phase = phase;
            session.parked = None;

            send(socket, &response, timeouts).await
        },
        Step::Park { phase, parked } => {
            session.phase = phase;
            session.parked = Some(parked);

            true
        },
        Step::Watch {
            response,
            phase,
            ticks,
        } => {
            session.phase = phase;
            session.ticks = Some(ticks);
            session.parked = None;

            send(socket, &response, timeouts).await
        },
        Step::Close(code) => {
            close(socket, code, "frame out of turn", timeouts).await;

            false
        },
    }
}

/// A parked pull re-attempts once the topic's window changes; `false` means the session ends.
async fn recheck(
    socket: &mut WebSocket,
    server: &Server,
    session: &mut Session,
    timeouts: Timeouts,
) -> bool {
    let Some(Parked::CaughtUp { max }) = session.parked else {
        return true;
    };

    apply(socket, server, session, Request::Pull { max }, timeouts).await
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

/// Waits until the watched topic's window changes.
///
/// The `None` branch cannot run, because only a parked session reaches this arm and a parked
/// session always holds a receiver. It stays because the alternative is an `unwrap` the lint set
/// denies.
async fn window_changed(ticks: &mut Option<watch::Receiver<Window>>) {
    match ticks {
        Some(ticks) => {
            let _ = ticks.changed().await;
        },
        None => std::future::pending().await,
    }
}

async fn handle(
    server: &Server,
    phase: &Phase,
    request: Request,
) -> Step {
    match (phase, request) {
        (Phase::Unopened, Request::Open { topic, subscriber }) => {
            open(server, topic, subscriber).await
        },
        (Phase::Publisher { topic, entry }, Request::Publish { payload }) => {
            publish(*topic, entry, payload).await
        },
        (
            Phase::Subscriber {
                topic,
                entry,
                subscriber,
                delivered,
                acked,
            },
            Request::Pull { max },
        ) => pull(server, *topic, entry, *subscriber, *delivered, *acked, max).await,
        (
            Phase::Subscriber {
                topic,
                entry,
                subscriber,
                delivered,
                acked,
            },
            Request::Ack { through },
        ) => {
            // Only the checkpoint moves here. Serving the freed backlog in the same breath would
            // put two responses behind one request, and a client reads exactly one.
            ack(
                server,
                *topic,
                entry,
                *subscriber,
                *delivered,
                *acked,
                through,
            )
            .await
        },
        _ => Step::Close(PROTOCOL_ERROR),
    }
}

async fn open(
    server: &Server,
    topic: Topic,
    subscriber: Option<SubscriberId>,
) -> Step {
    let (entry, ticks) = match server.bus.open_topic(topic).await {
        Ok(opened) => opened,
        Err(OpenError::Refused(reason)) => {
            tracing::warn!(%topic, %reason, "refused a topic");

            return Step::Answer {
                response: wire::Response::Refused { reason },
                phase: Phase::Unopened,
            };
        },
        Err(OpenError::Store(error)) => {
            tracing::error!(%error, "the store could not open the topic");

            return Step::Close(INTERNAL_ERROR);
        },
    };

    let window = *ticks.borrow();
    let phase = match subscriber {
        None => Phase::Publisher { topic, entry },
        Some(subscriber) => {
            let acked = match server.bus.store().checkpoint(subscriber, topic).await {
                Ok(None) => window.first(),
                Ok(Some(cursor)) if cursor.epoch() == window.epoch() => cursor.seq(),
                Ok(Some(_)) => {
                    tracing::warn!(%topic, "refused a subscriber from another epoch");

                    return Step::Answer {
                        response: wire::Response::Refused {
                            reason: Refusal::Unresumable { window },
                        },
                        phase: Phase::Unopened,
                    };
                },
                Err(error) => {
                    tracing::error!(%error, "the checkpoint could not be read");

                    return Step::Close(INTERNAL_ERROR);
                },
            };

            Phase::Subscriber {
                topic,
                entry,
                subscriber,
                delivered: acked,
                acked,
            }
        },
    };

    Step::Watch {
        response: wire::Response::Opened { window },
        phase,
        ticks,
    }
}

async fn publish(
    topic: Topic,
    entry: &Arc<TopicEntry>,
    payload: Payload,
) -> Step {
    match entry.append(topic, payload).await {
        Ok(published) => Step::Answer {
            response: wire::Response::from(published),
            phase: Phase::Publisher {
                topic,
                entry: Arc::clone(entry),
            },
        },
        Err(error) => {
            tracing::error!(%error, "the append failed");

            Step::Close(INTERNAL_ERROR)
        },
    }
}

async fn pull(
    server: &Server,
    topic: Topic,
    entry: &Arc<TopicEntry>,
    subscriber: SubscriberId,
    delivered: Seq,
    acked: Seq,
    max: NonZeroUsize,
) -> Step {
    let limits = server.config.limits;
    let max = max.min(limits.batch);
    let window = entry.window();

    let from = Seq::new(delivered.get().max(window.first().get()));
    let ceiling = acked.get().saturating_add(limits.in_flight.get() as u64);
    let credit = usize::try_from(ceiling.saturating_sub(from.get())).unwrap_or(usize::MAX);
    let count = max.get().min(credit);

    if count == 0 {
        if delivered < window.first() {
            // The checkpoint is behind the retained floor, so the events it names can never be
            // handed over. The gap travels on its own, with no event and no credit spent: the
            // caller acks the lapse, which moves its checkpoint to the floor and frees the credit.
            // Handing an event over instead would let a subscriber that never acks keep drawing
            // them, because eviction raises the floor the credit is measured against.
            return Step::Answer {
                response: wire::Response::Batch {
                    window,
                    lapsed: Some(Lapsed::new(
                        Cursor::new(window.epoch(), delivered),
                        window.first(),
                    )),
                    events: Vec::new(),
                },
                phase: Phase::Subscriber {
                    topic,
                    entry: Arc::clone(entry),
                    subscriber,
                    delivered: window.first(),
                    acked,
                },
            };
        }

        // Only an ack can free spent credit, so this park must not be woken by a commit.
        return Step::Park {
            phase: Phase::Subscriber {
                topic,
                entry: Arc::clone(entry),
                subscriber,
                delivered,
                acked,
            },
            parked: Parked::Spent,
        };
    }

    let count = NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN);

    match server.bus.store().read(topic, from, count).await {
        Ok((state, events)) if !events.is_empty() => {
            let next = events
                .last()
                .map_or(from, |event| Seq::new(event.seq().get() + 1));
            let window = Window::new(state.epoch, state.first, state.next);
            let lapsed = (delivered < state.first)
                .then(|| Lapsed::new(Cursor::new(state.epoch, delivered), state.first));

            Step::Answer {
                response: wire::Response::Batch {
                    window,
                    lapsed,
                    events,
                },
                phase: Phase::Subscriber {
                    topic,
                    entry: Arc::clone(entry),
                    subscriber,
                    delivered: next,
                    acked,
                },
            }
        },
        Ok(_) => Step::Park {
            phase: Phase::Subscriber {
                topic,
                entry: Arc::clone(entry),
                subscriber,
                delivered,
                acked,
            },
            parked: Parked::CaughtUp { max },
        },
        Err(error) => {
            tracing::error!(%error, "the read failed");

            Step::Close(INTERNAL_ERROR)
        },
    }
}

async fn ack(
    server: &Server,
    topic: Topic,
    entry: &Arc<TopicEntry>,
    subscriber: SubscriberId,
    delivered: Seq,
    acked: Seq,
    through: Cursor,
) -> Step {
    let window = entry.window();

    if through.epoch() != window.epoch() {
        // A stale frame is answered with the stored cursor, so the caller can see nothing moved.
        return Step::Answer {
            response: wire::Response::Acked {
                through: Cursor::new(window.epoch(), acked),
            },
            phase: Phase::Subscriber {
                topic,
                entry: Arc::clone(entry),
                subscriber,
                delivered,
                acked,
            },
        };
    }

    let clamped = through.seq().get().min(delivered.get()).max(acked.get());
    let through = Cursor::new(window.epoch(), Seq::new(clamped));

    match server.bus.store().advance(subscriber, topic, through).await {
        Ok(stored) => Step::Answer {
            response: wire::Response::Acked { through: stored },
            phase: Phase::Subscriber {
                topic,
                entry: Arc::clone(entry),
                subscriber,
                delivered,
                acked: stored.seq(),
            },
        },
        Err(error) => {
            tracing::error!(%error, "the checkpoint could not be advanced");

            Step::Close(INTERNAL_ERROR)
        },
    }
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
    use super::{Parked, Phase, ServeError, Server, SocketFile, Step, is_stale, pull, signals};
    use crate::bus::Bus;
    use crate::config::{Config, Limits, Retention};
    use crate::event::{Cursor, Seq};
    use crate::payload::Payload;
    use crate::subscriber::SubscriberId;
    use crate::topic::Topic;
    use crate::wire::Response;
    use proptest::prelude::*;
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
            "mokmok-bind-{}-{}-{name}",
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
                let Ok((entry, _ticks)) = bus.open_topic(Topic::Global).await else {
                    panic!("the topic could not be opened");
                };
                let server = Server {
                    bus: Arc::clone(&bus),
                    config,
                };
                let subscriber = SubscriberId::new();
                let max = NonZeroUsize::new(max).unwrap();
                let ceiling = acked.saturating_add(in_flight as u64);

                let step = pull(
                    &server,
                    Topic::Global,
                    &entry,
                    subscriber,
                    Seq::new(delivered),
                    Seq::new(acked),
                    max,
                )
                .await;

                match step {
                    Step::Answer {
                        response: Response::Batch { window, lapsed, events },
                        phase,
                    } => {
                        prop_assert!(events.len() <= max.get());
                        for pair in events.windows(2) {
                            prop_assert_eq!(pair[1].seq().get(), pair[0].seq().get() + 1);
                        }

                        let first = window.first().get();
                        let from = delivered.max(first);
                        prop_assert_eq!(lapsed.is_some(), delivered < first);
                        if let Some(lapsed) = lapsed {
                            prop_assert_eq!(
                                lapsed.held(),
                                Cursor::new(window.epoch(), Seq::new(delivered)),
                            );
                            prop_assert_eq!(lapsed.from(), window.first());
                        }

                        let Phase::Subscriber { delivered: next, .. } = phase else {
                            return Err(TestCaseError::fail(
                                "a batch left the subscriber phase",
                            ));
                        };

                        if events.is_empty() {
                            // Only the lapse travels alone, and the cursor it leaves behind is the
                            // floor, which is the ack that moves the checkpoint up and frees the
                            // credit. An empty batch without a lapse would be an answer with
                            // nothing in it.
                            prop_assert!(lapsed.is_some(), "an empty batch carried no lapse");
                            prop_assert_eq!(next.get(), first);
                        } else {
                            prop_assert_eq!(events.first().unwrap().seq().get(), from);
                            let last = events.last().unwrap().seq().get();
                            prop_assert_eq!(next.get(), last + 1);
                            prop_assert!(next.get() <= ceiling, "a batch ran past the credit");
                        }
                    },
                    Step::Answer { .. } => {
                        return Err(TestCaseError::fail(
                            "pull answered something other than a batch",
                        ));
                    },
                    Step::Park { phase, parked } => {
                        let window = entry.window();
                        let from = delivered.max(window.first().get());
                        prop_assert!(
                            from >= window.next().get() || delivered >= ceiling,
                            "pull parked although the read had events and credit remained",
                        );
                        // Spent is chosen exactly when the credit is gone, which is what keeps a
                        // commit from waking this park: only an Ack can free credit.
                        let spent = matches!(parked, Parked::Spent);
                        prop_assert_eq!(spent, delivered >= ceiling);
                        let still_subscribed = matches!(phase, Phase::Subscriber { .. });
                        prop_assert!(still_subscribed);
                    },
                    Step::Close(code) => {
                        return Err(TestCaseError::fail(format!(
                            "pull closed the session with {code}",
                        )));
                    },
                    Step::Watch { .. } => {
                        return Err(TestCaseError::fail("pull returned the watch step"));
                    },
                }

                bus.shutdown().await.unwrap();
                Ok::<(), TestCaseError>(())
            }).unwrap();
        }
    }
}

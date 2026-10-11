use std::fmt;
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

use crate::bus::{Bus, TopicEntry};
use crate::config::{Config, Timeouts};
use crate::event::{Position, Refusal, Window};
use crate::topic::Topic;
use crate::wire::{self, Request, RequestFrame};

const GOING_AWAY: u16 = 1001;
const PROTOCOL_ERROR: u16 = 1002;
const POLICY_VIOLATION: u16 = 1008;

#[derive(Debug)]
pub enum ServeError {
    AlreadyRunning,
    Io(io::Error),
}

/// Serves the bus on `config.path` until `shutdown` resolves, then unlinks the socket.
///
/// # Errors
///
/// Returns [`ServeError::AlreadyRunning`] when a live peer answers on the socket, and
/// [`ServeError::Io`] when the socket cannot be prepared or served.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let (socket, listener) = SocketFile::bind(&config.path).await?;
    let bus = Bus::new(config.clone());
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

    served.map_err(ServeError::Io)
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

impl fmt::Display for ServeError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::AlreadyRunning => {
                formatter.write_str("another process is already serving this socket")
            },
            Self::Io(error) => write!(formatter, "the socket could not be served: {error}"),
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyRunning => None,
            Self::Io(error) => Some(error),
        }
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
    Opened {
        topic: Topic,
        entry: Arc<TopicEntry>,
    },
    Publisher {
        topic: Topic,
        entry: Arc<TopicEntry>,
    },
    Subscriber {
        entry: Arc<TopicEntry>,
    },
}

/// Carries the values a frame changed back out of the handler, because a `select!` arm holds its
/// borrows while its handler runs.
enum Step {
    Answer {
        response: wire::Response,
        phase: Phase,
    },
    /// `bus.batch` had nothing to hand over, so the session waits for the topic's window.
    Park {
        phase: Phase,
        at: Position,
        max: NonZeroUsize,
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
    parked: Option<(Position, NonZeroUsize)>,
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
            () = window_changed(&mut session.ticks), if session.parked.is_some() => {
                if !recheck(&mut socket, &server.bus, &mut session, timeouts).await {
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

            apply(socket, &server.bus, session, request, timeouts).await
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
    bus: &Bus,
    session: &mut Session,
    request: Request,
    timeouts: Timeouts,
) -> bool {
    match handle(bus, &session.phase, request) {
        Step::Answer { response, phase } => {
            session.phase = phase;
            session.parked = None;

            send(socket, &response, timeouts).await
        },
        Step::Park { phase, at, max } => {
            session.phase = phase;
            session.parked = Some((at, max));

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
    bus: &Bus,
    session: &mut Session,
    timeouts: Timeouts,
) -> bool {
    let Some((position, max)) = session.parked else {
        return true;
    };

    apply(
        socket,
        bus,
        session,
        Request::Pull { position, max },
        timeouts,
    )
    .await
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

fn handle(
    bus: &Bus,
    phase: &Phase,
    request: Request,
) -> Step {
    match (phase, request) {
        (Phase::Unopened, Request::Open { topic }) => match bus.open(topic) {
            Ok((entry, ticks)) => {
                let window = *ticks.borrow();

                Step::Watch {
                    response: wire::Response::Opened { window },
                    phase: Phase::Opened { topic, entry },
                    ticks,
                }
            },
            Err(reason) => {
                tracing::warn!(%topic, %reason, "refused a topic");

                Step::Answer {
                    response: wire::Response::Refused { reason },
                    phase: Phase::Unopened,
                }
            },
        },
        (
            Phase::Opened { topic, entry } | Phase::Publisher { topic, entry },
            Request::Publish { payload },
        ) => Step::Answer {
            response: wire::Response::from(entry.append(*topic, payload)),
            phase: Phase::Publisher {
                topic: *topic,
                entry: Arc::clone(entry),
            },
        },
        (
            Phase::Opened { entry, .. } | Phase::Subscriber { entry },
            Request::Pull { position, max },
        ) => match bus.batch(entry, position, max) {
            Ok(batch) if batch.is_empty() && batch.lapsed().is_none() => Step::Park {
                phase: Phase::Subscriber {
                    entry: Arc::clone(entry),
                },
                at: position,
                max,
            },
            Ok(batch) => Step::Answer {
                response: wire::Response::from(batch),
                phase: Phase::Subscriber {
                    entry: Arc::clone(entry),
                },
            },
            Err(reason) => {
                tracing::warn!(%reason, "refused a pull");

                Step::Answer {
                    response: wire::Response::Refused { reason },
                    phase: Phase::Subscriber {
                        entry: Arc::clone(entry),
                    },
                }
            },
        },
        _ => Step::Close(PROTOCOL_ERROR),
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
) -> bool {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };

    write(socket, Message::Close(Some(frame)), timeouts).await
}

fn millis(value: NonZeroU32) -> Duration {
    Duration::from_millis(u64::from(value.get()))
}

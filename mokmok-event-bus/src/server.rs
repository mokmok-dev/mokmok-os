use std::fmt;
use std::future::Future;
use std::io;
use std::num::NonZeroU32;
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
use crate::event::{Refusal, Window};
use crate::topic::Topic;
use crate::wire::{self, Request};

/// The bus is going away.
const GOING_AWAY: u16 = 1001;
/// The peer broke the frame protocol.
const PROTOCOL_ERROR: u16 = 1002;
/// The peer stopped answering pings.
const POLICY_VIOLATION: u16 = 1008;
/// A frame the configured limit refuses.
const MESSAGE_TOO_BIG: u16 = 1009;

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

pub fn signals() -> impl Future<Output = ()> {
    interrupts()
}

/// Resolves on SIGINT or SIGTERM.
async fn interrupts() {
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
    upgrade.on_upgrade(move |socket| session(socket, server))
}

enum Phase {
    Unopened,
    Opened {
        topic: Topic,
        entry: Arc<TopicEntry>,
        ticks: watch::Receiver<Window>,
    },
    Publisher {
        topic: Topic,
    },
    Subscriber {
        entry: Arc<TopicEntry>,
        ticks: watch::Receiver<Window>,
    },
}

async fn session(
    mut socket: WebSocket,
    server: Server,
) {
    let Some(_permit) = server.bus.try_connect() else {
        tracing::warn!("refused a connection at the ceiling");
        let refused = wire::Response::Refused {
            reason: Refusal::TopicLimit,
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
    let frame_bytes = server.config.limits.frame_bytes.get();
    let mut stop = server.bus.stop_receiver();
    let mut heartbeat = tokio::time::interval(millis(timeouts.heartbeat));
    let mut pinged: Option<tokio::time::Instant> = None;
    let mut phase = Phase::Unopened;

    if *stop.borrow_and_update() {
        close(&mut socket, GOING_AWAY, "shutting down", timeouts).await;
        return;
    }

    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { return };

                match frame {
                    Message::Text(text) => {
                        if text.len() > frame_bytes {
                            tracing::warn!(
                                bytes = text.len(),
                                limit = frame_bytes,
                                "refused an oversized frame",
                            );
                            close(&mut socket, MESSAGE_TOO_BIG, "frame over the limit", timeouts).await;
                            return;
                        }

                        let Ok(request) = serde_json::from_str::<Request>(&text) else {
                            close(&mut socket, PROTOCOL_ERROR, "not a request frame", timeouts).await;
                            return;
                        };

                        match handle(&server.bus, phase, request, &mut stop).await {                            Ok((response, next)) => {
                                phase = next;
                                if !send(&mut socket, &response, timeouts).await {
                                    return;
                                }
                            },
                            Err(code) => {
                                close(&mut socket, code, "frame out of turn", timeouts).await;
                                return;
                            },
                        }
                    },
                    Message::Pong(_) => pinged = None,
                    Message::Close(_) => return,
                    // A ping needs no answer here: tungstenite replies while reading, and the
                    // bus speaks nothing but text.
                    Message::Ping(_) | Message::Binary(_) => {},
                }
            }
            _ = heartbeat.tick() => {
                if pinged.is_some_and(|sent| sent.elapsed() >= millis(timeouts.deadline)) {
                    tracing::warn!("closed a peer that stopped answering pings");
                    close(&mut socket, POLICY_VIOLATION, "no answer to a ping", timeouts).await;
                    return;
                }

                if pinged.is_none() {
                    if !write(&mut socket, Message::Ping(Bytes::new()), timeouts).await {
                        return;
                    }
                    pinged = Some(tokio::time::Instant::now());
                }
            }
            _ = stop.changed() => {
                close(&mut socket, GOING_AWAY, "shutting down", timeouts).await;
                return;
            }
        }
    }
}

async fn handle(
    bus: &Bus,
    phase: Phase,
    request: Request,
    stop: &mut watch::Receiver<bool>,
) -> Result<(wire::Response, Phase), u16> {
    match (phase, request) {
        (Phase::Unopened, Request::Open { topic }) => match bus.open(topic) {
            Ok((entry, ticks)) => {
                let window = *ticks.borrow();

                Ok((
                    wire::Response::Opened { window },
                    Phase::Opened {
                        topic,
                        entry,
                        ticks,
                    },
                ))
            },
            Err(reason) => {
                tracing::warn!(%topic, %reason, "refused a topic");

                Ok((wire::Response::Refused { reason }, Phase::Unopened))
            },
        },
        (
            Phase::Opened { topic, .. } | Phase::Publisher { topic },
            Request::Publish { payload },
        ) => match bus.append(topic, payload) {
            Ok(published) => Ok((
                wire::Response::Published {
                    id: published.id(),
                    seq: published.seq(),
                    at: published.at(),
                },
                Phase::Publisher { topic },
            )),
            Err(reason) => {
                tracing::warn!(%reason, "refused a publish");

                Ok((
                    wire::Response::Refused { reason },
                    Phase::Publisher { topic },
                ))
            },
        },
        (
            Phase::Opened {
                entry, mut ticks, ..
            }
            | Phase::Subscriber { entry, mut ticks },
            Request::Pull { position, max },
        ) => match bus.pull(&entry, &mut ticks, position, max, stop).await {
            Ok(batch) => Ok((
                wire::Response::from(batch),
                Phase::Subscriber { entry, ticks },
            )),
            Err(reason) => {
                tracing::warn!(%reason, "refused a pull");

                Ok((
                    wire::Response::Refused { reason },
                    Phase::Subscriber { entry, ticks },
                ))
            },
        },
        _ => Err(PROTOCOL_ERROR),
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

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::event::{Batch, Cursor, Published, Refusal};
use crate::payload::Payload;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;
use crate::wire::{self, Request, Response, SubId};

/// The path `serve` routes; the host is ignored for a socket the client opened itself.
const URL: &str = "ws://localhost/ws";

const FRAME: &str = "the bus sent a frame that is not a response";
const UNEXPECTED: &str = "the bus answered a request that was not made";
const MISMATCHED: &str = "the bus answered a request with the wrong kind of response";

#[derive(thiserror::Error)]
pub enum Error {
    #[error("the socket failed: {0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Protocol(&'static str),
    #[error("the bus refused: {0}")]
    Refused(Refusal),
    #[error("the bus closed the connection")]
    Closed,
    #[error("the publish was written but the bus never answered")]
    Indeterminate,
}

enum Command {
    Publish {
        topic: Topic,
        payload: Payload,
        reply: oneshot::Sender<Result<Outcome, Error>>,
    },
    Subscribe {
        topic: Topic,
        subscriber: SubscriberId,
        reply: oneshot::Sender<Result<Outcome, Error>>,
    },
    Pull {
        sub: SubId,
        reply: oneshot::Sender<Result<Outcome, Error>>,
    },
    Ack {
        sub: SubId,
        through: Cursor,
        reply: oneshot::Sender<Result<Outcome, Error>>,
    },
}

#[derive(Debug)]
enum Outcome {
    Published(Published),
    Subscribed(SubId),
    Batch(Batch),
    Acked,
}

struct Pending {
    request: Request,
    reply: oneshot::Sender<Result<Outcome, Error>>,
}

/// One connection to the bus that can both publish and subscribe.
#[derive(Clone)]
pub struct Client {
    sender: mpsc::UnboundedSender<Command>,
}

impl Client {
    /// Opens a WebSocket on `socket` and starts the driver task.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Refused`] when the bus rejects the handshake (for example, at its
    /// connection ceiling), and [`Error::Transport`] when the socket cannot be reached.
    pub async fn connect(socket: &Path) -> Result<Self, Error> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|error| Error::Transport(Box::new(error)))?;

        let (stream, _response) = tokio_tungstenite::client_async(URL, stream)
            .await
            .map_err(map_handshake)?;

        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(driver(stream, receiver));

        Ok(Self { sender })
    }

    /// Appends `payload` to `topic` and returns once the bus has stored it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Indeterminate`] when the frame was written but no readable answer came
    /// back: the append may or may not have happened, and only the caller can decide whether to
    /// retry. Every other failure names itself.
    pub async fn publish(
        &self,
        topic: Topic,
        payload: Payload,
    ) -> Result<Published, Error> {
        let (reply, answered) = oneshot::channel();
        self.send(Command::Publish {
            topic,
            payload,
            reply,
        })?;

        match answered.await.map_err(|_| Error::Closed)? {
            Ok(Outcome::Published(published)) => Ok(published),
            Ok(_) => Err(Error::Protocol(MISMATCHED)),
            Err(error) => Err(error),
        }
    }

    /// Opens a subscription to `topic` for `subscriber`. Two subscriptions on one topic are
    /// independent; two with the same `SubscriberId` share the durable checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Refused`] when the bus refuses the subscriber or topic, and the other
    /// errors a connection failure can produce.
    pub async fn subscribe(
        &self,
        topic: Topic,
        subscriber: SubscriberId,
    ) -> Result<Subscription, Error> {
        let (reply, answered) = oneshot::channel();
        self.send(Command::Subscribe {
            topic,
            subscriber,
            reply,
        })?;

        match answered.await.map_err(|_| Error::Closed)? {
            Ok(Outcome::Subscribed(sub)) => Ok(Subscription {
                client: self.clone(),
                sub,
            }),
            Ok(_) => Err(Error::Protocol(MISMATCHED)),
            Err(error) => Err(error),
        }
    }

    fn send(
        &self,
        command: Command,
    ) -> Result<(), Error> {
        self.sender.send(command).map_err(|_| Error::Closed)
    }
}

/// A handle to one subscription on a [`Client`]. `next` and `ack` take `&mut self` so a single
/// handle cannot issue two outstanding pulls.
pub struct Subscription {
    client: Client,
    sub: SubId,
}

impl Subscription {
    /// Reads the next delivery, waiting while the subscription has nothing newer to hand over.
    ///
    /// An answer that carries a [`Lapsed`](crate::Lapsed) and no events means the checkpoint fell
    /// behind the retained window, so the events it named can no longer be read. Acking that
    /// batch, which [`Batch::after`](crate::Batch::after) positions at the floor, moves the
    /// checkpoint up and frees the credit.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the bus refuses the held checkpoint, closes the connection, or if
    /// the socket fails.
    pub async fn next(&mut self) -> Result<Batch, Error> {
        let (reply, answered) = oneshot::channel();
        self.client.send(Command::Pull {
            sub: self.sub,
            reply,
        })?;

        match answered.await.map_err(|_| Error::Closed)? {
            Ok(Outcome::Batch(batch)) => Ok(batch),
            Ok(_) => Err(Error::Protocol(MISMATCHED)),
            Err(error) => Err(error),
        }
    }

    /// Advances the checkpoint to `through`.
    ///
    /// Ack only after the effects of those events are durable. The checkpoint is advanced
    /// server-side, and a crash before the ack replays the batch, so an ack that names work not
    /// yet done turns a duplicate into a loss.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the bus closes the connection or if the socket fails.
    pub async fn ack(
        &mut self,
        through: Cursor,
    ) -> Result<(), Error> {
        let (reply, answered) = oneshot::channel();
        self.client.send(Command::Ack {
            sub: self.sub,
            through,
            reply,
        })?;

        match answered.await.map_err(|_| Error::Closed)? {
            Ok(Outcome::Acked) => Ok(()),
            Ok(_) => Err(Error::Protocol(MISMATCHED)),
            Err(error) => Err(error),
        }
    }
}

fn map_handshake(error: tokio_tungstenite::tungstenite::Error) -> Error {
    use tokio_tungstenite::tungstenite::Error as WsError;

    let WsError::Http(response) = error else {
        return Error::Transport(Box::new(error));
    };

    if response.status() != 503 {
        return Error::Transport(Box::new(WsError::Http(response)));
    }

    let body = response
        .body()
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();

    Refusal::from_upgrade_body(&body).map_or_else(
        // A 503 this client does not understand stays a transport failure: reporting it as a
        // shutdown would name a cause the bus never stated.
        || Error::Transport(Box::new(WsError::Http(response))),
        Error::Refused,
    )
}

async fn driver(
    mut stream: WebSocketStream<UnixStream>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut next_id: u64 = 1;
    let mut outstanding: HashMap<u64, Pending> = HashMap::new();

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    fail_rest(&mut outstanding, &Ending::Clean);
                    return;
                };
                if !send_command(
                    &mut stream,
                    &mut next_id,
                    &mut outstanding,
                    command,
                ).await {
                    return;
                }
            }
            frame = stream.next() => {
                if !receive_frame(
                    &mut outstanding,
                    frame,
                ) {
                    return;
                }
            }
        }
    }
}

async fn send_command(
    stream: &mut WebSocketStream<UnixStream>,
    next_id: &mut u64,
    outstanding: &mut HashMap<u64, Pending>,
    command: Command,
) -> bool {
    let req_id = *next_id;
    *next_id += 1;

    let (request, reply) = match command {
        Command::Publish {
            topic,
            payload,
            reply,
        } => (
            Request::Publish {
                req_id,
                topic,
                payload,
            },
            reply,
        ),
        Command::Subscribe {
            topic,
            subscriber,
            reply,
        } => (
            Request::Subscribe {
                req_id,
                topic,
                subscriber,
            },
            reply,
        ),
        Command::Pull { sub, reply } => (
            Request::Pull {
                req_id,
                sub,
                max: NonZeroUsize::MAX,
            },
            reply,
        ),
        Command::Ack {
            sub,
            through,
            reply,
        } => (
            Request::Ack {
                req_id,
                sub,
                through,
            },
            reply,
        ),
    };

    let Ok(text) = serde_json::to_string(&request) else {
        let _ = reply.send(Err(Error::Protocol("the request could not be encoded")));
        return true;
    };

    let pending = Pending { request, reply };
    let _ = outstanding.insert(req_id, pending);

    match stream.send(Message::Text(text.into())).await {
        Ok(()) => true,
        Err(error) => {
            let detail = error.to_string();
            if let Some(current) = outstanding.remove(&req_id) {
                let _ = current.reply.send(Err(Error::Transport(Box::new(error))));
            }
            fail_rest(outstanding, &Ending::Lost(detail));
            false
        },
    }
}

fn receive_frame(
    outstanding: &mut HashMap<u64, Pending>,
    frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> bool {
    match frame {
        None | Some(Ok(Message::Close(_))) => {
            fail_rest(outstanding, &Ending::Clean);
            false
        },
        Some(Err(error)) => {
            fail_rest(outstanding, &Ending::Lost(error.to_string()));
            false
        },
        Some(Ok(Message::Text(text))) => {
            let Some(response) = wire::ResponseFrame::decode(text.as_str()) else {
                fail_all(outstanding, FRAME);
                return false;
            };

            let req_id = response.req_id();
            let Some(pending) = outstanding.remove(&req_id) else {
                fail_all(outstanding, UNEXPECTED);
                return false;
            };

            let outcome = if let Response::Refused { reason, .. } = response {
                Err(Error::Refused(reason))
            } else if let Ok(outcome) = resolve(&pending.request, response) {
                Ok(outcome)
            } else {
                fail_all(outstanding, MISMATCHED);
                return false;
            };

            let _ = pending.reply.send(outcome);
            true
        },
        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {
            true
        },
    }
}

fn resolve(
    request: &Request,
    response: Response,
) -> Result<Outcome, Error> {
    match (request, response) {
        (Request::Publish { .. }, Response::Published { id, seq, at, .. }) => {
            Ok(Outcome::Published(Published::new(id, seq, at)))
        },
        (Request::Subscribe { .. }, Response::Subscribed { sub, .. }) => {
            Ok(Outcome::Subscribed(sub))
        },
        (
            Request::Pull { .. },
            Response::Batch {
                window,
                lapsed,
                events,
                ..
            },
        ) => Ok(Outcome::Batch(Batch::new(window, lapsed, events))),
        (Request::Ack { .. }, Response::Acked { .. }) => Ok(Outcome::Acked),
        _ => Err(Error::Protocol(MISMATCHED)),
    }
}

fn fail_all(
    outstanding: &mut HashMap<u64, Pending>,
    message: &'static str,
) {
    for (_req_id, pending) in outstanding.drain() {
        let _ = pending.reply.send(Err(Error::Protocol(message)));
    }
}

/// How the connection ended, for requests that were already written.
enum Ending {
    /// The peer closed the WebSocket, or the last client handle dropped.
    Clean,
    /// The socket failed; the text names what happened for callers whose request was not a
    /// publish.
    Lost(String),
}

fn fail_rest(
    outstanding: &mut HashMap<u64, Pending>,
    ending: &Ending,
) {
    for (_req_id, pending) in outstanding.drain() {
        let error = if matches!(pending.request, Request::Publish { .. }) {
            Error::Indeterminate
        } else {
            match ending {
                Ending::Clean => Error::Closed,
                Ending::Lost(detail) => {
                    Error::Transport(Box::new(io::Error::other(detail.clone())))
                },
            }
        };
        let _ = pending.reply.send(Err(error));
    }
}

// Manual rather than derived so a failure prints through `Display`: the derive would render the
// boxed source's internals, and `Refused` as its variant name instead of the bus's explanation.
impl fmt::Debug for Error {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "Transport({error})"),
            Self::Protocol(message) => write!(formatter, "Protocol({message})"),
            Self::Refused(reason) => write!(formatter, "Refused({reason})"),
            Self::Closed => formatter.write_str("Closed"),
            Self::Indeterminate => formatter.write_str("Indeterminate"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ending, FRAME, Pending, fail_all, fail_rest, map_handshake, receive_frame};
    use crate::event::Refusal;
    use crate::payload::Payload;
    use crate::topic::Topic;
    use crate::wire::{Request, SubId};
    use std::collections::HashMap;
    use std::io;
    use std::num::NonZeroUsize;
    use tokio::sync::oneshot;
    use tokio_tungstenite::tungstenite::Error as WsError;
    use tokio_tungstenite::tungstenite::Message;

    fn publish(req_id: u64) -> Request {
        Request::Publish {
            req_id,
            topic: Topic::Global,
            payload: Payload::json(&1).unwrap(),
        }
    }

    fn outstanding(
        request: Request
    ) -> (
        HashMap<u64, Pending>,
        oneshot::Receiver<Result<super::Outcome, super::Error>>,
    ) {
        let (reply, answered) = oneshot::channel();
        let req_id = match &request {
            Request::Publish { req_id, .. }
            | Request::Subscribe { req_id, .. }
            | Request::Pull { req_id, .. }
            | Request::Ack { req_id, .. } => *req_id,
        };
        let mut pending = HashMap::new();
        pending.insert(req_id, Pending { request, reply });

        (pending, answered)
    }

    #[test]
    fn a_written_publish_is_indeterminate_and_another_written_request_is_closed() {
        let (mut pending, mut answered) = outstanding(publish(1));
        fail_rest(&mut pending, &Ending::Clean);

        assert!(matches!(
            answered.try_recv(),
            Ok(Err(super::Error::Indeterminate)),
        ));

        let (mut pending, mut answered) = outstanding(Request::Ack {
            req_id: 2,
            sub: SubId::new(0),
            through: crate::event::Cursor::new(
                crate::event::LogEpoch::new(uuid::Uuid::nil()),
                crate::event::Seq::new(0),
            ),
        });
        fail_rest(&mut pending, &Ending::Clean);

        assert!(matches!(answered.try_recv(), Ok(Err(super::Error::Closed)),));
    }

    #[test]
    fn a_lost_socket_names_the_failure_for_a_request_that_was_not_a_publish() {
        let (mut pending, mut answered) = outstanding(Request::Pull {
            req_id: 3,
            sub: SubId::new(0),
            max: NonZeroUsize::MIN,
        });
        fail_rest(&mut pending, &Ending::Lost("the pipe broke".to_owned()));

        let error = answered.try_recv().unwrap().unwrap_err();
        assert_eq!(error.to_string(), "the socket failed: the pipe broke");
    }

    #[test]
    fn a_bad_frame_answers_every_outstanding_request_and_ends_the_driver() {
        let (mut pending, mut answered) = outstanding(publish(1));

        assert!(!receive_frame(
            &mut pending,
            Some(Ok(Message::Text("not a frame".into()))),
        ));
        assert!(matches!(
            answered.try_recv(),
            Ok(Err(super::Error::Protocol(FRAME))),
        ));
    }

    #[test]
    fn a_closed_socket_fails_the_outstanding_request() {
        let (mut pending, mut answered) = outstanding(publish(1));

        assert!(!receive_frame(&mut pending, None));
        assert!(matches!(
            answered.try_recv(),
            Ok(Err(super::Error::Indeterminate)),
        ));
    }

    #[test]
    fn a_protocol_failure_answers_every_outstanding_request() {
        let (mut pending, mut answered) = outstanding(publish(1));

        fail_all(&mut pending, FRAME);

        assert!(matches!(
            answered.try_recv(),
            Ok(Err(super::Error::Protocol(FRAME))),
        ));
    }

    fn http_503(body: &'static [u8]) -> WsError {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(503)
            .body(Some(body.to_vec()))
            .unwrap();

        WsError::Http(Box::new(response))
    }

    #[test]
    fn a_503_body_names_the_refusal_it_carries() {
        assert!(matches!(
            map_handshake(http_503(b"connection_limit")),
            super::Error::Refused(Refusal::ConnectionLimit),
        ));
        assert!(matches!(
            map_handshake(http_503(b"shutting_down")),
            super::Error::Refused(Refusal::ShuttingDown),
        ));
    }

    #[test]
    fn an_unknown_503_body_and_another_status_stay_transport_failures() {
        assert!(matches!(
            map_handshake(http_503(b"else")),
            super::Error::Transport(_),
        ));

        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(403)
            .body(None)
            .unwrap();
        assert!(matches!(
            map_handshake(WsError::Http(Box::new(response))),
            super::Error::Transport(_),
        ));
    }

    #[test]
    fn a_transport_error_with_no_http_response_stays_transport() {
        assert!(matches!(
            map_handshake(WsError::Io(io::Error::other("gone"))),
            super::Error::Transport(_),
        ));
    }
}

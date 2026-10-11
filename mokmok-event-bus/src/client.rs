use std::fmt;
use std::num::NonZeroUsize;
use std::path::Path;

use futures_util::SinkExt;
use futures_util::StreamExt;
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::event::{Batch, Cursor, Published, Refusal};
use crate::payload::Payload;
use crate::subscriber::SubscriberId;
use crate::topic::Topic;
use crate::wire::{Request, Response, ResponseFrame};

/// The path `serve` routes; the host is ignored for a socket the client opened itself.
const URL: &str = "ws://localhost/ws";

const UNEXPECTED: &str = "the bus answered a request that was not made";

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

pub struct Publisher {
    stream: WebSocketStream<UnixStream>,
}

pub struct Subscriber {
    stream: WebSocketStream<UnixStream>,
}

impl Publisher {
    /// Opens a connection and names the one topic every later publish appends to.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the socket cannot be reached, if the bus refuses the topic, or
    /// if the bus closes the connection.
    pub async fn open(
        socket: &Path,
        topic: Topic,
    ) -> Result<Self, Error> {
        Ok(Self {
            stream: open(socket, topic, None).await?,
        })
    }

    /// Appends `payload` and returns once the bus has stored it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Indeterminate`] when the frame was written but no readable answer came
    /// back: the append may or may not have happened, and only the caller can decide whether to
    /// retry. Every other failure names itself.
    pub async fn publish(
        &mut self,
        payload: Payload,
    ) -> Result<Published, Error> {
        send(&mut self.stream, &Request::Publish { payload }).await?;

        match receive(&mut self.stream).await {
            Ok(Some(Response::Published { id, seq, at })) => Ok(Published::new(id, seq, at)),
            Ok(Some(Response::Refused { reason })) => Err(Error::Refused(reason)),
            Ok(Some(_)) => Err(Error::Protocol(UNEXPECTED)),
            Ok(None) | Err(_) => Err(Error::Indeterminate),
        }
    }
}

impl Subscriber {
    /// Opens a connection and resumes `subscriber`'s checkpoint on `topic`. A fresh id starts at
    /// the oldest retained event; an id whose checkpoint names another epoch is refused with
    /// [`Refusal::Unresumable`].
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the socket cannot be reached, if the bus refuses the subscriber,
    /// or if the bus closes the connection.
    pub async fn open(
        socket: &Path,
        topic: Topic,
        subscriber: SubscriberId,
    ) -> Result<Self, Error> {
        Ok(Self {
            stream: open(socket, topic, Some(subscriber)).await?,
        })
    }

    /// Reads the next delivery, waiting for one while the topic has nothing newer to hand over.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the bus refuses the held checkpoint, closes the connection, or if
    /// the socket fails.
    pub async fn next(&mut self) -> Result<Batch, Error> {
        // Asking for as much as the bus will give keeps the client free of a second place that
        // has to know the batch limit; the bus clamps the request to its own configuration and its
        // in-flight ceiling.
        let max = NonZeroUsize::MAX;

        match exchange(&mut self.stream, &Request::Pull { max }).await? {
            Some(Response::Batch {
                window,
                lapsed,
                events,
            }) => Ok(Batch::new(window, lapsed, events)),
            Some(Response::Refused { reason }) => Err(Error::Refused(reason)),
            Some(_) => Err(Error::Protocol(UNEXPECTED)),
            None => Err(Error::Closed),
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
        match exchange(&mut self.stream, &Request::Ack { through }).await? {
            Some(Response::Acked { .. }) => Ok(()),
            Some(Response::Refused { reason }) => Err(Error::Refused(reason)),
            Some(_) => Err(Error::Protocol(UNEXPECTED)),
            None => Err(Error::Closed),
        }
    }
}

async fn connect(socket: &Path) -> Result<WebSocketStream<UnixStream>, Error> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|error| Error::Transport(Box::new(error)))?;

    let (stream, _) = tokio_tungstenite::client_async(URL, stream)
        .await
        .map_err(|error| Error::Transport(Box::new(error)))?;

    Ok(stream)
}

/// Opens a session and names its topic. The bus refuses a connection at its ceiling before it
/// reads anything, so a failed write is not fatal here: the refusal may already be waiting, and
/// dropping it would turn a typed refusal into a broken pipe.
async fn open(
    socket: &Path,
    topic: Topic,
    subscriber: Option<SubscriberId>,
) -> Result<WebSocketStream<UnixStream>, Error> {
    let mut stream = connect(socket).await?;
    let written = send(&mut stream, &Request::Open { topic, subscriber }).await;

    let answered = match receive(&mut stream).await {
        Ok(answered) => answered,
        Err(error) => return Err(written.err().unwrap_or(error)),
    };

    match answered {
        Some(Response::Opened { .. }) => Ok(stream),
        Some(Response::Refused { reason }) => Err(Error::Refused(reason)),
        Some(_) => Err(Error::Protocol(UNEXPECTED)),
        None => Err(Error::Closed),
    }
}

async fn exchange(
    stream: &mut WebSocketStream<UnixStream>,
    request: &Request,
) -> Result<Option<Response>, Error> {
    send(stream, request).await?;
    receive(stream).await
}

async fn send(
    stream: &mut WebSocketStream<UnixStream>,
    request: &Request,
) -> Result<(), Error> {
    let text = serde_json::to_string(request)
        .map_err(|_| Error::Protocol("the request could not be encoded"))?;

    stream
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| Error::Transport(Box::new(error)))
}

/// Reads the frame that answers the last request; `None` means the peer closed the connection.
async fn receive(stream: &mut WebSocketStream<UnixStream>) -> Result<Option<Response>, Error> {
    loop {
        match stream.next().await {
            None | Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Err(error)) => return Err(Error::Transport(Box::new(error))),
            Some(Ok(Message::Text(text))) => {
                let Some(response) = ResponseFrame::decode(text.as_str()) else {
                    return Err(Error::Protocol(
                        "the bus sent a frame that is not a response",
                    ));
                };

                return Ok(Some(response));
            },
            // A ping is answered by tungstenite's read loop, which flushes the queued pong on
            // the next read, so the client never has to write one itself.
            Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            )) => {},
        }
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

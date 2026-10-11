use std::fmt;
use std::num::NonZeroUsize;
use std::path::Path;

use futures_util::SinkExt;
use futures_util::StreamExt;
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::event::{Batch, Position, Published, Refusal};
use crate::payload::Payload;
use crate::topic::Topic;
use crate::wire::{Request, Response};

/// The path `serve` routes; the host is ignored for a socket the client opened itself.
const URL: &str = "ws://localhost/ws";

const UNEXPECTED: &str = "the bus answered a request that was not made";

pub enum Error {
    Transport(Box<dyn std::error::Error + Send + Sync>),
    Protocol(&'static str),
    Refused(Refusal),
    Closed,
    Indeterminate,
}

pub struct Publisher {
    stream: WebSocketStream<UnixStream>,
}

pub struct Subscriber {
    stream: WebSocketStream<UnixStream>,
    position: Position,
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
            stream: open(socket, topic).await?,
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
    /// Opens a connection and reads the topic from `at`, which is also how a restart resumes.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the socket cannot be reached, if the bus refuses the position,
    /// or if the bus closes the connection.
    pub async fn open(
        socket: &Path,
        topic: Topic,
        at: Position,
    ) -> Result<Self, Error> {
        Ok(Self {
            stream: open(socket, topic).await?,
            position: at,
        })
    }

    /// Reads the next delivery, waiting for one while the log has nothing newer to hand over.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the bus refuses the held position, closes the connection, or if
    /// the socket fails.
    pub async fn next(&mut self) -> Result<Batch, Error> {
        // Asking for as much as the bus will give keeps the client free of a second place that
        // has to know the batch limit; the bus clamps the request to its own configuration.
        let max = NonZeroUsize::MAX;

        let answered = exchange(
            &mut self.stream,
            &Request::Pull {
                position: self.position,
                max,
            },
        )
        .await;

        match answered? {
            Some(Response::Batch {
                window,
                lapsed,
                events,
            }) => {
                let batch = Batch::new(window, lapsed, events);
                self.position = batch.after();

                Ok(batch)
            },
            Some(Response::Refused { reason }) => Err(Error::Refused(reason)),
            Some(_) => Err(Error::Protocol(UNEXPECTED)),
            None => Err(Error::Closed),
        }
    }

    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
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
) -> Result<WebSocketStream<UnixStream>, Error> {
    let mut stream = connect(socket).await?;
    let written = send(&mut stream, &Request::Open { topic }).await;

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
/// Anything that is not a response frame, the bus's pings included, is skipped.
async fn receive(stream: &mut WebSocketStream<UnixStream>) -> Result<Option<Response>, Error> {
    loop {
        match stream.next().await {
            None | Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Err(error)) => return Err(Error::Transport(Box::new(error))),
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str())
                    .map(Some)
                    .map_err(|_| Error::Protocol("the bus sent a frame that is not a response"));
            },
            // A ping is answered by tungstenite's read loop, which flushes the queued pong on
            // the next read, so the client never has to write one itself.
            Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            )) => {},
        }
    }
}

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

impl fmt::Display for Error {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "the socket failed: {error}"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Refused(reason) => write!(formatter, "the bus refused: {reason}"),
            Self::Closed => formatter.write_str("the bus closed the connection"),
            Self::Indeterminate => {
                formatter.write_str("the publish was written but the bus never answered")
            },
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(&**error),
            Self::Protocol(_) | Self::Refused(_) | Self::Closed | Self::Indeterminate => None,
        }
    }
}

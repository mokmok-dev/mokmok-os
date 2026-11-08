//! The message channel an ACP connection runs over.

mod stdio;
mod websocket;

use std::fmt;

pub use stdio::{StdioConfig, StdioTransport};
pub use websocket::{WebSocketConfig, WebSocketTransport};

/// A duplex channel of JSON-RPC messages.
///
/// One JSON value travels per message on both built-in transports. A custom transport is free to
/// frame however its wire does, as long as it preserves message boundaries.
pub trait Transport: Send {
    /// Sends one JSON-RPC message.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Closed`] when the peer has gone, and
    /// [`TransportError::Io`] or [`TransportError::WebSocket`] when the write fails.
    fn send(
        &mut self,
        message: &str,
    ) -> Result<(), TransportError>;

    /// Receives one JSON-RPC message, or `Ok(None)` when the peer closed the channel cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Io`] or [`TransportError::WebSocket`] when reading fails.
    fn recv(&mut self) -> Result<Option<String>, TransportError>;

    /// Receives one JSON-RPC message, waiting at most `timeout`.
    ///
    /// The default ignores the timeout and blocks, so a transport that cannot bound a read
    /// still compiles and behaves as before.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Timeout`] when no message arrived in time, and the same
    /// errors as [`Transport::recv`] otherwise.
    fn recv_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<String>, TransportError> {
        let _ = timeout;
        self.recv()
    }

    /// Closes the channel. Calling it more than once is harmless.
    ///
    /// # Errors
    ///
    /// Returns the error the transport reports while closing. A transport that cannot report one
    /// returns `Ok(())`.
    fn close(&mut self) -> Result<(), TransportError>;
}

/// Why a transport could not carry a message.
#[derive(Debug)]
pub enum TransportError {
    /// The configured URL is not `ws://` or `wss://`.
    UnsupportedScheme(String),
    /// The URL, a header name, or a header value could not be parsed.
    InvalidRequest(String),
    /// The channel is closed, or the child process is gone.
    Closed,
    /// No message arrived before the read deadline.
    Timeout,
    /// Reading or writing the channel failed.
    Io(std::io::Error),
    /// The WebSocket handshake or frame exchange failed.
    WebSocket(String),
}

impl fmt::Display for TransportError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported WebSocket scheme {scheme:?}")
            },
            Self::InvalidRequest(message) => write!(formatter, "invalid request: {message}"),
            Self::Closed => formatter.write_str("transport is closed"),
            Self::Timeout => formatter.write_str("the transport timed out"),
            Self::Io(error) => write!(formatter, "transport I/O failed: {error}"),
            Self::WebSocket(message) => write!(formatter, "WebSocket failed: {message}"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::UnsupportedScheme(_)
            | Self::InvalidRequest(_)
            | Self::Closed
            | Self::Timeout
            | Self::WebSocket(_) => None,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_io_error_has_a_source() {
        let io = TransportError::Io(std::io::Error::other("x"));
        assert!(std::error::Error::source(&io).is_some());
        assert!(std::error::Error::source(&TransportError::Closed).is_none());
        assert!(std::error::Error::source(&TransportError::Timeout).is_none());
        assert!(std::error::Error::source(&TransportError::WebSocket("x".to_owned())).is_none());
        assert!(
            std::error::Error::source(&TransportError::InvalidRequest("x".to_owned())).is_none()
        );
        assert!(
            std::error::Error::source(&TransportError::UnsupportedScheme("x".to_owned())).is_none()
        );
    }
}

//! An ACP agent reached over a WebSocket.

use tungstenite::Message;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderName, HeaderValue, Uri};
use url::Url;

use super::{Transport, TransportError};

/// How to reach an ACP agent through a WebSocket.
///
/// The URL is a normal `ws://` or `wss://` URL. Each JSON-RPC message travels as one text frame.
/// [`WebSocketConfig::grok`] adds the `server-key` query parameter Grok Build's reference server
/// prints, and [`WebSocketConfig::bearer`] adds an `Authorization: Bearer` header, which that server
/// also accepts.
#[derive(Clone, Debug)]
pub struct WebSocketConfig {
    url: String,
    secret: Option<String>,
    headers: Vec<(String, String)>,
}

impl WebSocketConfig {
    /// Connects to `url` with no authentication.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            secret: None,
            headers: Vec::new(),
        }
    }

    /// Connects to the URL Grok Build's `grok agent serve` prints.
    ///
    /// The secret is carried as the `server-key` query parameter, which is what the server's
    /// printed URL uses.
    #[must_use]
    pub fn grok(
        url: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self::new(url).server_key(secret)
    }

    /// Adds the `server-key` query parameter. An existing one is replaced.
    #[must_use]
    pub fn server_key(
        mut self,
        secret: impl Into<String>,
    ) -> Self {
        self.secret = Some(secret.into());
        self
    }

    /// Adds an `Authorization: Bearer` header.
    #[must_use]
    pub fn bearer(
        mut self,
        token: impl Into<String>,
    ) -> Self {
        self.headers.push((
            "authorization".to_owned(),
            format!("Bearer {}", token.into()),
        ));
        self
    }

    /// Adds a header to the opening handshake.
    #[must_use]
    pub fn header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

/// An ACP agent reached through a WebSocket.
pub struct WebSocketTransport {
    socket: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    closed: bool,
}

impl WebSocketTransport {
    /// Opens the WebSocket, including the handshake and any configured authentication.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::UnsupportedScheme`] for a URL that is not `ws://` or `wss://`,
    /// [`TransportError::InvalidRequest`] when the URL or a header is malformed, and
    /// [`TransportError::WebSocket`] when the handshake fails.
    pub fn connect(config: &WebSocketConfig) -> Result<Self, TransportError> {
        let url = Url::parse(config.url())
            .map_err(|error| TransportError::InvalidRequest(error.to_string()))?;
        let scheme = url.scheme().to_owned();
        if scheme != "ws" && scheme != "wss" {
            return Err(TransportError::UnsupportedScheme(scheme));
        }
        let url = with_secret(url, config.secret());
        let uri: Uri =
            url.as_str()
                .parse()
                .map_err(|error: tungstenite::http::uri::InvalidUri| {
                    TransportError::InvalidRequest(error.to_string())
                })?;
        let mut request = uri
            .into_client_request()
            .map_err(|error| TransportError::InvalidRequest(error.to_string()))?;
        for (name, value) in config.headers() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| TransportError::InvalidRequest(error.to_string()))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| TransportError::InvalidRequest(error.to_string()))?;
            request.headers_mut().insert(name, value);
        }
        let (socket, _) = tungstenite::connect(request)
            .map_err(|error| TransportError::WebSocket(error.to_string()))?;
        Ok(Self {
            socket,
            closed: false,
        })
    }
}

impl Transport for WebSocketTransport {
    fn send(
        &mut self,
        message: &str,
    ) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        match self.socket.send(Message::text(message)) {
            Ok(()) => Ok(()),
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Err(TransportError::Closed)
            },
            Err(error) => Err(TransportError::WebSocket(error.to_string())),
        }
    }

    fn recv(&mut self) -> Result<Option<String>, TransportError> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(text.as_str().to_owned())),
                Ok(Message::Close(_))
                | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = true;
                    return Ok(None);
                },
                Ok(Message::Ping(_)) => {
                    if let Err(error) = self.socket.flush() {
                        return Err(TransportError::WebSocket(error.to_string()));
                    }
                },
                Ok(Message::Binary(_) | Message::Pong(_) | Message::Frame(_)) => {},
                Err(error) => return Err(TransportError::WebSocket(error.to_string())),
            }
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let closed = self.socket.close(None).and_then(|()| self.socket.flush());
        match closed {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            },
            Err(error) => Err(TransportError::WebSocket(error.to_string())),
        }
    }
}

fn with_secret(
    mut url: Url,
    secret: Option<&str>,
) -> Url {
    let Some(secret) = secret else {
        return url;
    };
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key.as_ref() != "server-key")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("server-key", secret);
    url
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use tungstenite::handshake::server::{Request, Response};

    use super::*;

    #[allow(clippy::result_large_err)]
    fn serve(message: &'static str) -> (String, Receiver<Request>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let (requests, received) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut socket =
                tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
                    requests.send(request.clone()).expect("handshake request");
                    Ok(response)
                })
                .expect("handshake");
            socket.send(Message::text(message)).expect("send");
            let _ = socket.read();
        });
        (format!("ws://{address}/ws"), received, handle)
    }

    #[test]
    fn the_server_key_replaces_an_existing_one_and_keeps_other_parameters() {
        let (url, received, handle) = serve(r#"{"jsonrpc":"2.0","id":1}"#);
        let config = WebSocketConfig::grok(format!("{url}?other=1&server-key=old"), "s3cret");
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        assert_eq!(
            transport.recv().expect("message").as_deref(),
            Some(r#"{"jsonrpc":"2.0","id":1}"#)
        );
        transport.close().expect("close");
        transport.close().expect("second close");
        drop(transport);
        let request = received.recv().expect("handshake request");
        let query = request.uri().query().expect("query");
        assert!(query.contains("other=1"), "{query}");
        assert!(query.contains("server-key=s3cret"), "{query}");
        assert!(!query.contains("server-key=old"), "{query}");
        handle.join().expect("server");
    }

    #[test]
    fn a_bearer_token_reaches_the_handshake() {
        let (url, received, handle) = serve(r#"{"jsonrpc":"2.0","id":2}"#);
        let config = WebSocketConfig::new(url).bearer("s3cret");
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        assert_eq!(
            transport.recv().expect("message").as_deref(),
            Some(r#"{"jsonrpc":"2.0","id":2}"#)
        );
        transport.close().expect("close");
        drop(transport);
        let request = received.recv().expect("handshake request");
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .expect("authorization header"),
            "Bearer s3cret"
        );
        handle.join().expect("server");
    }

    #[test]
    fn another_scheme_is_refused() {
        assert!(matches!(
            WebSocketTransport::connect(&WebSocketConfig::new("http://127.0.0.1:9/ws")),
            Err(TransportError::UnsupportedScheme(scheme)) if scheme == "http"
        ));
    }

    #[test]
    fn a_malformed_header_is_refused_before_connecting() {
        let config = WebSocketConfig::new("ws://127.0.0.1:9/ws").header("bad name", "value");
        assert!(matches!(
            WebSocketTransport::connect(&config),
            Err(TransportError::InvalidRequest(_))
        ));
    }

    #[test]
    fn the_builders_store_their_values() {
        let config = WebSocketConfig::new("ws://host/ws")
            .server_key("s")
            .header("x-test", "1")
            .bearer("t");
        assert_eq!(config.url(), "ws://host/ws");
        assert_eq!(config.secret(), Some("s"));
        assert_eq!(config.headers().len(), 2);
        assert_eq!(config.headers()[0], ("x-test".to_owned(), "1".to_owned()));
        assert_eq!(
            config.headers()[1],
            ("authorization".to_owned(), "Bearer t".to_owned())
        );
    }
}

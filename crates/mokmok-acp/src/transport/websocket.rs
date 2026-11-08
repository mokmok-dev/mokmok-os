//! An ACP agent reached over a WebSocket.

use std::fmt;
use std::time::Duration;

use tungstenite::Message;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderName, HeaderValue, Uri};
use tungstenite::stream::MaybeTlsStream;
use url::Url;

use super::{Transport, TransportError};

/// What a failed frame exchange reports. The detailed `tungstenite` error may name the
/// peer, so it never leaves this module.
const FAILED: &str = "the WebSocket failed";

// A connect failure keeps a category so a mistyped host, a refused port, and a bad
// certificate stay distinguishable. The URL, its query, and the host stay out.
fn connect_error(error: &tungstenite::Error) -> TransportError {
    let category = match error {
        tungstenite::Error::Io(error) => match error.kind() {
            std::io::ErrorKind::ConnectionRefused => "was refused",
            std::io::ErrorKind::TimedOut => "timed out",
            _ => "is unreachable",
        },
        tungstenite::Error::Tls(_) => "failed the TLS handshake",
        tungstenite::Error::Http(response) if response.status() == 401 => "refused the handshake",
        tungstenite::Error::Http(_) => "failed the handshake",
        _ => "is unreachable",
    };
    TransportError::WebSocket(format!("the agent endpoint {category}"))
}

/// How to reach an ACP agent through a WebSocket.
///
/// The URL is a normal `ws://` or `wss://` URL. Each JSON-RPC message travels as one text frame.
/// [`WebSocketConfig::grok`] adds the `server-key` query parameter Grok Build's reference server
/// prints, and [`WebSocketConfig::bearer`] adds an `Authorization: Bearer` header, which that server
/// also accepts.
#[derive(Clone)]
pub struct WebSocketConfig {
    url: String,
    secret: Option<String>,
    headers: Vec<(String, String)>,
}

impl fmt::Debug for WebSocketConfig {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter
            .debug_struct("WebSocketConfig")
            .field("url", &redacted_url(&self.url))
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

// A URL may carry the `server-key` query the caller built it with, so only the
// scheme, authority, and path are ever printed.
fn redacted_url(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return "<invalid url>".to_owned();
    };
    let host = url.host_str().unwrap_or("<no host>");
    let authority = url
        .port()
        .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
    format!("{}://{authority}{}", url.scheme(), url.path())
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
            .map_err(|_| TransportError::InvalidRequest("the agent URL is not valid".to_owned()))?;
        let scheme = url.scheme().to_owned();
        if scheme != "ws" && scheme != "wss" {
            return Err(TransportError::UnsupportedScheme(scheme));
        }
        let url = with_secret(url, config.secret());
        let uri: Uri = url
            .as_str()
            .parse()
            .map_err(|_: tungstenite::http::uri::InvalidUri| {
                TransportError::InvalidRequest("the agent URL is not a valid URI".to_owned())
            })?;
        let mut request = uri.into_client_request().map_err(|_| {
            TransportError::InvalidRequest("the agent URL is not a valid request".to_owned())
        })?;
        for (name, value) in config.headers() {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                TransportError::InvalidRequest("an agent header name is malformed".to_owned())
            })?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                TransportError::InvalidRequest("an agent header value is malformed".to_owned())
            })?;
            request.headers_mut().insert(name, value);
        }
        let (socket, _) = tungstenite::connect(request).map_err(|error| connect_error(&error))?;
        Ok(Self {
            socket,
            closed: false,
        })
    }

    fn set_read_timeout(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<(), TransportError> {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
            MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
            // This build enables rustls only; any other stream keeps its blocking read.
            _ => Ok(()),
        }
        .map_err(TransportError::Io)
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
            Err(_) => Err(TransportError::WebSocket(FAILED.to_owned())),
        }
    }

    fn recv(&mut self) -> Result<Option<String>, TransportError> {
        loop {
            match self.recv_timeout(Duration::from_millis(50)) {
                Err(TransportError::Timeout) => {},
                other => return other,
            }
        }
    }

    fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<String>, TransportError> {
        // A zero budget has nothing to wait for, and set_read_timeout refuses it.
        if timeout.is_zero() {
            return Err(TransportError::Timeout);
        }
        self.set_read_timeout(Some(timeout))?;
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(text.as_str().to_owned())),
                Ok(Message::Close(_))
                | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = true;
                    return Ok(None);
                },
                Ok(Message::Ping(_)) => {
                    if self.socket.flush().is_err() {
                        return Err(TransportError::WebSocket(FAILED.to_owned()));
                    }
                },
                Ok(Message::Binary(_) | Message::Pong(_) | Message::Frame(_)) => {},
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(TransportError::Timeout);
                },
                Err(_) => return Err(TransportError::WebSocket(FAILED.to_owned())),
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
            Err(_) => Err(TransportError::WebSocket(FAILED.to_owned())),
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
    fn recv_timeout_reports_a_quiet_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut socket = tungstenite::accept(stream).expect("handshake");
            let _ = socket.read();
        });
        let config = WebSocketConfig::new(format!("ws://{address}/ws"));
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        assert!(matches!(
            transport.recv_timeout(Duration::from_millis(50)),
            Err(TransportError::Timeout)
        ));
        assert!(matches!(
            transport.recv_timeout(Duration::ZERO),
            Err(TransportError::Timeout)
        ));
        transport.close().expect("close");
        drop(transport);
        handle.join().expect("server");
    }

    #[test]
    fn a_closed_socket_is_reported_as_closed_not_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut socket = tungstenite::accept(stream).expect("handshake");
            socket.close(None).expect("the server closes the socket");
            let _ = socket.flush();
            let _ = socket.read();
        });
        let config = WebSocketConfig::new(format!("ws://{address}/ws"));
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        assert!(matches!(
            transport.recv_timeout(Duration::from_millis(500)),
            Ok(None)
        ));
        drop(transport);
        handle.join().expect("server");
    }

    #[test]
    fn a_dropped_socket_is_not_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let socket = tungstenite::accept(stream).expect("handshake");
            drop(socket);
        });
        let config = WebSocketConfig::new(format!("ws://{address}/ws"));
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        let result = transport.recv_timeout(Duration::from_millis(500));
        assert!(
            !matches!(result, Err(TransportError::Timeout)),
            "a dropped socket must not look like a quiet one: {result:?}"
        );
        drop(transport);
        handle.join().expect("server");
    }

    #[test]
    fn a_connect_failure_does_not_echo_the_secret() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let config = WebSocketConfig::grok(format!("ws://{address}/ws"), "s3cret");
        let Err(error) = WebSocketTransport::connect(&config) else {
            panic!("nobody listens");
        };
        let message = error.to_string();
        assert!(!message.contains("s3cret"), "{message}");
        assert!(message.contains("the agent endpoint"), "{message}");
    }

    #[test]
    fn a_connect_error_names_a_category_without_a_url() {
        use std::io::{Error, ErrorKind};

        use tungstenite::Error as WsError;
        use tungstenite::error::{ProtocolError, TlsError};

        fn message(error: &WsError) -> String {
            match connect_error(error) {
                TransportError::WebSocket(message) => message,
                other => panic!("expected a WebSocket error, got {other:?}"),
            }
        }

        assert_eq!(
            message(&WsError::Io(Error::from(ErrorKind::ConnectionRefused))),
            "the agent endpoint was refused"
        );
        assert_eq!(
            message(&WsError::Io(Error::from(ErrorKind::TimedOut))),
            "the agent endpoint timed out"
        );
        assert_eq!(
            message(&WsError::Io(Error::other("x"))),
            "the agent endpoint is unreachable"
        );
        assert_eq!(
            message(&WsError::Tls(TlsError::InvalidDnsName)),
            "the agent endpoint failed the TLS handshake"
        );
        let unauthorized = tungstenite::http::Response::builder()
            .status(401)
            .body(None)
            .expect("a response");
        assert_eq!(
            message(&WsError::Http(Box::new(unauthorized))),
            "the agent endpoint refused the handshake"
        );
        let server_error = tungstenite::http::Response::builder()
            .status(500)
            .body(None)
            .expect("a response");
        assert_eq!(
            message(&WsError::Http(Box::new(server_error))),
            "the agent endpoint failed the handshake"
        );
        assert_eq!(
            message(&WsError::Protocol(
                ProtocolError::ResetWithoutClosingHandshake
            )),
            "the agent endpoint is unreachable"
        );
    }

    #[test]
    fn debug_redacts_the_secret_and_the_query() {
        let config = WebSocketConfig::new("wss://host/ws?server-key=old")
            .server_key("s3cret")
            .bearer("token");
        let debug = format!("{config:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(!debug.contains("token"), "{debug}");
        assert!(!debug.contains("server-key"), "{debug}");
        assert!(debug.contains("wss://host/ws"), "{debug}");
    }

    #[test]
    fn a_reset_socket_is_a_websocket_error_not_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let socket = tungstenite::accept(stream).expect("handshake");
            socket2::SockRef::from(socket.get_ref())
                .set_linger(Some(Duration::ZERO))
                .expect("linger zero");
            drop(socket);
        });
        let config = WebSocketConfig::new(format!("ws://{address}/ws"));
        let mut transport = WebSocketTransport::connect(&config).expect("connect");
        let result = transport.recv_timeout(Duration::from_millis(500));
        assert!(
            matches!(result, Err(TransportError::WebSocket(_))),
            "a reset socket must not look like a quiet one: {result:?}"
        );
        drop(transport);
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

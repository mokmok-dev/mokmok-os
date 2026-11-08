//! A task delegated over a WebSocket, in the shape Grok Build's server uses.

// The handshake callback's error type belongs to tungstenite; this test cannot shrink it.
#![allow(clippy::result_large_err)]

mod common;

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::{Recorder, TestError, TestResult, within_deadline};
use mokmok_acp::{
    AcpClient, AcpError, ConnectionConfig, StopReason, TransportError, WebSocketConfig,
};
use tungstenite::handshake::server::{Request, Response};
use tungstenite::{Message, WebSocket};

type ThreadResult = Result<(), String>;

struct Seen {
    uri: Arc<Mutex<Option<String>>>,
    authorization: Arc<Mutex<Option<String>>>,
}

struct Server {
    listener: TcpListener,
    seen: Seen,
}

impl Server {
    fn start() -> Result<(Self, thread::JoinHandle<ThreadResult>), TestError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let thread_listener = listener.try_clone()?;
        let seen = Seen {
            uri: Arc::new(Mutex::new(None)),
            authorization: Arc::new(Mutex::new(None)),
        };
        let uri = Arc::clone(&seen.uri);
        let authorization = Arc::clone(&seen.authorization);
        let handle = thread::spawn(move || -> ThreadResult {
            let (stream, _) = thread_listener
                .accept()
                .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            let socket =
                tungstenite::accept_hdr(stream, move |request: &Request, response: Response| {
                    if let Ok(mut uri) = uri.lock() {
                        *uri = Some(request.uri().to_string());
                    }
                    let authorization_value = request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    if let Ok(mut authorization) = authorization.lock() {
                        *authorization = authorization_value;
                    }
                    Ok(response)
                })
                .map_err(|error| error.to_string())?;
            let mut link = WsLink { socket };
            common::run_agent(&mut link);
            Ok(())
        });
        Ok((Self { listener, seen }, handle))
    }

    fn url(&self) -> Result<String, TestError> {
        let address = self.listener.local_addr()?;
        Ok(format!("ws://{address}/ws"))
    }

    fn uri(&self) -> Option<String> {
        self.seen.uri.lock().ok().and_then(|uri| uri.clone())
    }

    fn authorization(&self) -> Option<String> {
        self.seen
            .authorization
            .lock()
            .ok()
            .and_then(|authorization| authorization.clone())
    }
}

struct WsLink {
    socket: WebSocket<std::net::TcpStream>,
}

impl common::Link for WsLink {
    fn send(
        &mut self,
        message: &str,
    ) {
        let _ = self.socket.send(Message::Text(message.to_owned().into()));
    }

    fn recv(&mut self) -> Option<String> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Some(text.to_string()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {},
            }
        }
    }
}

#[test]
fn a_task_runs_with_the_server_key_query() -> TestResult {
    within_deadline(|| {
        let (server, handle) = Server::start()?;
        let config = ConnectionConfig::WebSocket(WebSocketConfig::grok(server.url()?, "s3cret"));
        let mut client = AcpClient::open(config)?;
        let uri = server.uri().ok_or("the handshake did not arrive")?;
        assert!(uri.contains("server-key=s3cret"), "uri was {uri}");
        let cwd = std::env::current_dir()?;
        let session = client.new_session(&cwd)?;
        let mut handler = Recorder::default();
        let turn = client.delegate(&session, "do the task", &mut handler)?;
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        assert_eq!(handler.updates, 1);
        assert!(server.authorization().is_none());
        client.close()?;
        assert!(matches!(
            client.cancel(&session),
            Err(AcpError::Transport(TransportError::Closed))
        ));
        drop(client);
        handle
            .join()
            .map_err(|_| "the server thread panicked".to_owned())??;
        Ok(())
    })
}

#[test]
fn a_task_runs_with_a_bearer_header() -> TestResult {
    within_deadline(|| {
        let (server, handle) = Server::start()?;
        let config =
            ConnectionConfig::WebSocket(WebSocketConfig::new(server.url()?).bearer("s3cret"));
        let mut client = AcpClient::open(config)?;
        assert_eq!(server.authorization().as_deref(), Some("Bearer s3cret"));
        let cwd = std::env::current_dir()?;
        let session = client.new_session(&cwd)?;
        let mut handler = Recorder::default();
        let turn = client.delegate(&session, "do the task", &mut handler)?;
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        client.close()?;
        drop(client);
        handle
            .join()
            .map_err(|_| "the server thread panicked".to_owned())??;
        Ok(())
    })
}

//! The client that drives one ACP connection.

use std::fmt;
use std::path::{Path, PathBuf};

use acp::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, CancelNotification, ClientCapabilities, ContentBlock,
    Implementation, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, ProtocolVersion, RequestId, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionId, SessionNotification,
    StopReason, TextContent,
};
use agent_client_protocol_schema as acp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::jsonrpc::{self, Message};
use crate::transport::{
    StdioConfig, StdioTransport, Transport, TransportError, WebSocketConfig, WebSocketTransport,
};

/// How to reach an ACP agent.
#[derive(Clone, Debug)]
pub enum ConnectionConfig {
    /// Launch the agent as a child process and speak newline-delimited JSON over its pipes.
    Stdio(StdioConfig),
    /// Connect to an agent that is already listening on a WebSocket.
    WebSocket(WebSocketConfig),
}

/// An initialized connection to one ACP agent.
///
/// [`AcpClient::open`] performs the `initialize` handshake, so every value of this type has a
/// negotiated protocol version and capabilities. A session is created with
/// [`AcpClient::new_session`], and one task runs with [`AcpClient::delegate`].
pub struct AcpClient {
    connection: Connection,
    agent: InitializeResponse,
}

/// An ACP session bound to one working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    cwd: PathBuf,
}

/// One finished prompt turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    /// The session the turn ran in.
    pub session_id: SessionId,
    /// Why the agent stopped.
    pub stop_reason: StopReason,
}

/// One task to hand to an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// The absolute working directory the agent works in.
    pub cwd: PathBuf,
    /// The text sent as the user prompt.
    pub prompt: String,
}

/// The client side of an ACP connection.
///
/// The agent calls a handler for streamed updates and for permission decisions. Every other
/// agent-to-client request is answered with `method not found`, because this client advertises no
/// filesystem or terminal capabilities.
pub trait Handler {
    /// Receives one streamed session update.
    fn session_update(
        &mut self,
        update: SessionNotification,
    );

    /// Decides one permission request. The default refuses the request by cancelling it.
    fn request_permission(
        &mut self,
        _request: RequestPermissionRequest,
    ) -> RequestPermissionResponse {
        RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
    }
}

/// A handler that ignores updates and refuses every permission request.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopHandler;

impl Handler for NoopHandler {
    fn session_update(
        &mut self,
        _update: SessionNotification,
    ) {
    }
}

/// Why an ACP connection could not complete an operation.
#[derive(Debug)]
pub enum AcpError {
    /// The transport failed.
    Transport(TransportError),
    /// The peer sent something that is not a JSON-RPC 2.0 message, or a known message that does
    /// not deserialize.
    Protocol(String),
    /// The peer closed the connection.
    Closed,
    /// The agent answered a request with an error.
    Agent(acp::Error),
    /// The agent negotiated a protocol version newer than this client understands.
    UnsupportedVersion(ProtocolVersion),
    /// The working directory handed to [`AcpClient::new_session`] is relative.
    InvalidCwd(PathBuf),
}

impl fmt::Display for AcpError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "{error}"),
            Self::Protocol(message) => write!(formatter, "ACP protocol error: {message}"),
            Self::Closed => formatter.write_str("the agent closed the connection"),
            Self::Agent(error) => write!(formatter, "the agent refused: {}", error.message),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "the agent speaks ACP {version}, this client speaks {}",
                ProtocolVersion::LATEST
            ),
            Self::InvalidCwd(path) => {
                write!(
                    formatter,
                    "the session cwd {} is not absolute",
                    path.display()
                )
            },
        }
    }
}

impl std::error::Error for AcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Protocol(_)
            | Self::Closed
            | Self::Agent(_)
            | Self::UnsupportedVersion(_)
            | Self::InvalidCwd(_) => None,
        }
    }
}

impl From<TransportError> for AcpError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<serde_json::Error> for AcpError {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl Session {
    /// The protocol identifier for this session.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// The working directory the session was created for.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl Task {
    /// Builds a task for `cwd`.
    #[must_use]
    pub fn new(
        cwd: impl Into<PathBuf>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            prompt: prompt.into(),
        }
    }
}

impl AcpClient {
    /// Connects to an agent and performs the `initialize` handshake.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Transport`] when the connection cannot be opened, and
    /// [`AcpError::Protocol`], [`AcpError::Closed`], [`AcpError::Agent`], or
    /// [`AcpError::UnsupportedVersion`] when the handshake fails.
    pub fn open(config: ConnectionConfig) -> Result<Self, AcpError> {
        let mut connection = Connection::open(config)?;
        let mut handler = NoopHandler;
        let agent = connection.initialize(&mut handler)?;
        Ok(Self { connection, agent })
    }

    /// The agent's handshake response.
    #[must_use]
    pub const fn agent(&self) -> &InitializeResponse {
        &self.agent
    }

    /// Creates a session in an absolute working directory.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::InvalidCwd`] when `cwd` is relative, and the same errors as
    /// [`AcpClient::open`] when the request fails.
    pub fn new_session(
        &mut self,
        cwd: impl AsRef<Path>,
    ) -> Result<Session, AcpError> {
        let cwd = cwd.as_ref();
        if !cwd.is_absolute() {
            return Err(AcpError::InvalidCwd(cwd.to_path_buf()));
        }
        let mut handler = NoopHandler;
        let response = self.connection.new_session(cwd, &mut handler)?;
        Ok(Session {
            id: response.session_id,
            cwd: cwd.to_path_buf(),
        })
    }

    /// Sends one prompt and streams the turn's updates to `handler` until the agent stops.
    ///
    /// Updates for any session on this connection reach the handler, so a handler that cares
    /// about one session checks [`SessionNotification::session_id`].
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Agent`] when the agent answers with an error, and
    /// [`AcpError::Closed`] or [`AcpError::Transport`] when the connection fails mid-turn.
    pub fn delegate(
        &mut self,
        session: &Session,
        prompt: impl Into<String>,
        handler: &mut dyn Handler,
    ) -> Result<Turn, AcpError> {
        let request = PromptRequest::new(
            session.id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt))],
        );
        let params = to_value(&request)?;
        let value =
            self.connection
                .call(AGENT_METHOD_NAMES.session_prompt, Some(&params), handler)?;
        let response: PromptResponse = from_value(value)?;
        Ok(Turn {
            session_id: session.id.clone(),
            stop_reason: response.stop_reason,
        })
    }

    /// Asks the agent to stop the session's in-flight turn.
    ///
    /// The agent answers the in-flight prompt with [`StopReason::Cancelled`]. A synchronous caller
    /// cannot send this while [`AcpClient::delegate`] is blocked; it is for a client that owns the
    /// connection from another thread.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Transport`] when the notification cannot be sent.
    pub fn cancel(
        &mut self,
        session: &Session,
    ) -> Result<(), AcpError> {
        let request = CancelNotification::new(session.id.clone());
        let params = to_value(&request)?;
        self.connection
            .notify(AGENT_METHOD_NAMES.session_cancel, Some(&params))
    }

    /// Closes the connection. Dropping the client does the same.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Transport`] when the transport reports a failure while closing.
    pub fn close(&mut self) -> Result<(), AcpError> {
        self.connection.close()
    }
}

/// Opens one connection, runs one task, and closes it.
///
/// # Errors
///
/// Returns the first error from [`AcpClient::open`], [`AcpClient::new_session`], or
/// [`AcpClient::delegate`].
pub fn delegate_task(
    config: ConnectionConfig,
    task: &Task,
    handler: &mut dyn Handler,
) -> Result<Turn, AcpError> {
    let mut client = AcpClient::open(config)?;
    let session = client.new_session(&task.cwd)?;
    client.delegate(&session, task.prompt.clone(), handler)
}

struct Connection {
    transport: Box<dyn Transport>,
    next_id: i64,
}

impl Connection {
    fn open(config: ConnectionConfig) -> Result<Self, AcpError> {
        let transport: Box<dyn Transport> = match config {
            ConnectionConfig::Stdio(config) => Box::new(StdioTransport::spawn(&config)?),
            ConnectionConfig::WebSocket(config) => Box::new(WebSocketTransport::connect(&config)?),
        };
        Ok(Self {
            transport,
            next_id: 1,
        })
    }

    fn initialize(
        &mut self,
        handler: &mut dyn Handler,
    ) -> Result<InitializeResponse, AcpError> {
        let request = InitializeRequest::new(ProtocolVersion::LATEST)
            .client_capabilities(ClientCapabilities::new())
            .client_info(Implementation::new("mokmok-os", env!("CARGO_PKG_VERSION")));
        let params = to_value(&request)?;
        let value = self.call(AGENT_METHOD_NAMES.initialize, Some(&params), handler)?;
        let response: InitializeResponse = from_value(value)?;
        if response.protocol_version > ProtocolVersion::LATEST {
            return Err(AcpError::UnsupportedVersion(response.protocol_version));
        }
        Ok(response)
    }

    fn new_session(
        &mut self,
        cwd: &Path,
        handler: &mut dyn Handler,
    ) -> Result<NewSessionResponse, AcpError> {
        let request = NewSessionRequest::new(cwd.to_path_buf());
        let params = to_value(&request)?;
        let value = self.call(AGENT_METHOD_NAMES.session_new, Some(&params), handler)?;
        from_value(value)
    }

    fn call(
        &mut self,
        method: &str,
        params: Option<&Value>,
        handler: &mut dyn Handler,
    ) -> Result<Value, AcpError> {
        let id = RequestId::Number(self.next_id);
        self.next_id += 1;
        let line = jsonrpc::request(&id, method, params)?;
        self.transport.send(&line).map_err(AcpError::Transport)?;
        loop {
            let line = self
                .transport
                .recv()
                .map_err(AcpError::Transport)?
                .ok_or(AcpError::Closed)?;
            match jsonrpc::parse(&line).map_err(AcpError::Protocol)? {
                Message::Response {
                    id: response,
                    result,
                    error,
                } if response == id => {
                    if let Some(error) = error {
                        return Err(AcpError::Agent(error));
                    }
                    return Ok(result.unwrap_or(Value::Null));
                },
                Message::Response { .. } => {},
                message => self.dispatch(message, handler)?,
            }
        }
    }

    fn notify(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<(), AcpError> {
        let line = jsonrpc::notification(method, params)?;
        self.transport.send(&line).map_err(AcpError::Transport)
    }

    fn dispatch(
        &mut self,
        message: Message,
        handler: &mut dyn Handler,
    ) -> Result<(), AcpError> {
        match message {
            Message::Notification { method, params }
                if method == CLIENT_METHOD_NAMES.session_update =>
            {
                let notification: SessionNotification = from_value(params.unwrap_or(Value::Null))?;
                handler.session_update(notification);
            },
            Message::Notification { .. } | Message::Response { .. } => {},
            Message::Request { id, method, params } => {
                self.answer(&id, &method, params, handler)?;
            },
        }
        Ok(())
    }

    fn answer(
        &mut self,
        id: &RequestId,
        method: &str,
        params: Option<Value>,
        handler: &mut dyn Handler,
    ) -> Result<(), AcpError> {
        let line = if method == CLIENT_METHOD_NAMES.session_request_permission {
            match from_value::<RequestPermissionRequest>(params.unwrap_or(Value::Null)) {
                Ok(request) => {
                    let response = handler.request_permission(request);
                    jsonrpc::response(id, &to_value(&response)?)?
                },
                Err(_) => jsonrpc::error_response(id, &acp::Error::invalid_params())?,
            }
        } else {
            jsonrpc::error_response(id, &acp::Error::method_not_found())?
        };
        self.transport.send(&line).map_err(AcpError::Transport)
    }

    fn close(&mut self) -> Result<(), AcpError> {
        self.transport.close().map_err(AcpError::Transport)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn to_value(value: &impl Serialize) -> Result<Value, AcpError> {
    serde_json::to_value(value).map_err(AcpError::from)
}

fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, AcpError> {
    serde_json::from_value(value).map_err(AcpError::from)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::error::Error as _;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[derive(Clone, Default)]
    struct Recorded {
        incoming: Arc<Mutex<VecDeque<String>>>,
        sent: Arc<Mutex<Vec<String>>>,
        closed: Arc<Mutex<bool>>,
    }

    struct MockTransport {
        recorded: Recorded,
    }

    impl Transport for MockTransport {
        fn send(
            &mut self,
            message: &str,
        ) -> Result<(), TransportError> {
            if *self.recorded.closed.lock().expect("closed lock") {
                return Err(TransportError::Closed);
            }
            self.recorded
                .sent
                .lock()
                .expect("sent lock")
                .push(message.to_owned());
            Ok(())
        }

        fn recv(&mut self) -> Result<Option<String>, TransportError> {
            Ok(self
                .recorded
                .incoming
                .lock()
                .expect("incoming lock")
                .pop_front())
        }

        fn close(&mut self) -> Result<(), TransportError> {
            *self.recorded.closed.lock().expect("closed lock") = true;
            Ok(())
        }
    }

    fn mock(script: Vec<Value>) -> (Connection, Recorded) {
        let recorded = Recorded::default();
        recorded
            .incoming
            .lock()
            .expect("incoming lock")
            .extend(script.into_iter().map(|value| value.to_string()));
        let connection = Connection {
            transport: Box::new(MockTransport {
                recorded: recorded.clone(),
            }),
            next_id: 1,
        };
        (connection, recorded)
    }

    fn client(script: Vec<Value>) -> (AcpClient, Recorded) {
        let (mut connection, recorded) = mock(script);
        let mut handler = NoopHandler;
        let agent = connection.initialize(&mut handler).expect("initialize");
        (AcpClient { connection, agent }, recorded)
    }

    fn sent(recorded: &Recorded) -> Vec<Value> {
        recorded
            .sent
            .lock()
            .expect("sent lock")
            .iter()
            .map(|line| serde_json::from_str(line).expect("sent line is JSON"))
            .collect()
    }

    fn initialize_response() -> Value {
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}})
    }

    #[test]
    fn open_sends_initialize_and_accepts_version_one() {
        let (mut connection, recorded) = mock(vec![initialize_response()]);
        let mut handler = NoopHandler;
        let response = connection
            .initialize(&mut handler)
            .expect("version one is accepted");
        assert_eq!(response.protocol_version, ProtocolVersion::LATEST);
        let sent = sent(&recorded);
        assert_eq!(sent[0]["method"], "initialize");
        assert_eq!(sent[0]["params"]["protocolVersion"], 1);
        assert_eq!(sent[0]["params"]["clientInfo"]["name"], "mokmok-os");
    }

    #[test]
    fn a_newer_agent_version_is_refused() {
        let (mut connection, _) = mock(vec![
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}}),
        ]);
        let mut handler = NoopHandler;
        match connection.initialize(&mut handler) {
            Err(AcpError::UnsupportedVersion(version)) => {
                assert!(version > ProtocolVersion::LATEST);
            },
            other => panic!("expected an unsupported version, got {other:?}"),
        }
    }

    #[test]
    fn new_session_requires_an_absolute_path() {
        let (mut client, _) = client(vec![initialize_response()]);
        assert!(matches!(
            client.new_session("relative"),
            Err(AcpError::InvalidCwd(path)) if path == Path::new("relative")
        ));
    }

    #[test]
    fn new_session_sends_the_cwd_and_returns_the_id() {
        let (mut client, recorded) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
        ]);
        let session = client.new_session("/tmp").expect("session");
        assert_eq!(session.id().to_string(), "s-1");
        assert_eq!(session.cwd(), Path::new("/tmp"));
        let sent = sent(&recorded);
        assert_eq!(sent[1]["method"], "session/new");
        assert_eq!(sent[1]["params"]["cwd"], "/tmp");
    }

    #[derive(Default)]
    struct Recorder {
        updates: Vec<SessionNotification>,
        permissions: Vec<RequestPermissionRequest>,
    }

    impl Handler for Recorder {
        fn session_update(
            &mut self,
            update: SessionNotification,
        ) {
            self.updates.push(update);
        }

        fn request_permission(
            &mut self,
            request: RequestPermissionRequest,
        ) -> RequestPermissionResponse {
            self.permissions.push(request);
            RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
        }
    }

    #[test]
    fn delegate_streams_updates_answers_permission_and_returns_the_stop_reason() {
        let (mut client, recorded) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
            json!({"jsonrpc":"2.0","method":"x.ai/notice","params":{}}),
            json!({
                "jsonrpc":"2.0",
                "method":"session/update",
                "params":{
                    "sessionId":"s-1",
                    "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working"}}
                }
            }),
            json!({
                "jsonrpc":"2.0",
                "id":99,
                "method":"session/request_permission",
                "params":{
                    "sessionId":"s-1",
                    "toolCall":{"toolCallId":"t-1"},
                    "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
                }
            }),
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
        ]);
        let session = client.new_session("/tmp").expect("session");
        let mut handler = Recorder::default();
        let turn = client
            .delegate(&session, "do it", &mut handler)
            .expect("delegated");
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        assert_eq!(handler.updates.len(), 1);
        assert_eq!(handler.permissions.len(), 1);
        let sent = sent(&recorded);
        let prompt = sent
            .iter()
            .find(|message| message["method"] == "session/prompt")
            .expect("prompt sent");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "do it");
        let answer = sent
            .iter()
            .find(|message| message["id"] == 99)
            .expect("permission answered");
        assert_eq!(answer["result"]["outcome"], json!({"outcome":"cancelled"}));
    }

    #[test]
    fn an_agent_error_is_returned() {
        let (mut client, _) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "error":{"code":-32603,"message":"boom"}
            }),
        ]);
        let session = client.new_session("/tmp").expect("session");
        let mut handler = NoopHandler;
        assert!(matches!(
            client.delegate(&session, "do it", &mut handler),
            Err(AcpError::Agent(error)) if error.message == "boom"
        ));
    }

    #[test]
    fn a_closed_transport_is_reported() {
        let (mut client, _) = client(vec![initialize_response()]);
        assert!(matches!(client.new_session("/tmp"), Err(AcpError::Closed)));
    }

    #[test]
    fn an_unknown_agent_request_gets_method_not_found() {
        let (mut client, recorded) = client(vec![
            initialize_response(),
            json!({
                "jsonrpc":"2.0",
                "id":77,
                "method":"fs/read_text_file",
                "params":{"sessionId":"s","path":"/tmp/x"}
            }),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
        ]);
        client.new_session("/tmp").expect("session");
        let sent = sent(&recorded);
        let answer = sent
            .iter()
            .find(|message| message["id"] == 77)
            .expect("unknown request answered");
        assert_eq!(answer["error"]["code"], -32601);
    }

    #[test]
    fn cancel_sends_a_notification() {
        let (mut client, recorded) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
        ]);
        let session = client.new_session("/tmp").expect("session");
        client.cancel(&session).expect("cancelled");
        let sent = sent(&recorded);
        let cancel = sent
            .iter()
            .find(|message| message["method"] == "session/cancel")
            .expect("cancel sent");
        assert_eq!(cancel["params"]["sessionId"], "s-1");
        assert!(cancel.get("id").is_none());
    }

    #[test]
    fn a_non_jsonrpc_line_is_a_protocol_error() {
        let (mut client, _) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"1.0","id":2,"result":{}}),
        ]);
        assert!(matches!(
            client.new_session("/tmp"),
            Err(AcpError::Protocol(_))
        ));
    }

    #[test]
    fn dropping_the_client_closes_the_transport() {
        let (connection, recorded) = mock(vec![initialize_response()]);
        drop(connection);
        assert!(*recorded.closed.lock().expect("closed lock"));
    }

    #[test]
    fn a_stale_response_is_ignored() {
        let (mut client, _) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":99,"result":{"sessionId":"wrong"}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
        ]);
        let session = client.new_session("/tmp").expect("session");
        assert_eq!(session.id().to_string(), "s-1");
    }

    #[test]
    fn closing_twice_is_harmless_and_rejects_later_requests() {
        let (mut client, _) = client(vec![
            initialize_response(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}),
        ]);
        client.close().expect("first close");
        client.close().expect("second close");
        assert!(matches!(
            client.new_session("/tmp"),
            Err(AcpError::Transport(TransportError::Closed))
        ));
    }

    #[test]
    fn the_noop_handler_cancels_permission() {
        let request: RequestPermissionRequest = serde_json::from_value(json!({
            "sessionId":"s",
            "toolCall":{"toolCallId":"t-1"},
            "options":[]
        }))
        .expect("request");
        let mut handler = NoopHandler;
        assert_eq!(
            handler.request_permission(request).outcome,
            RequestPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn errors_describe_themselves() {
        assert_eq!(
            AcpError::Closed.to_string(),
            "the agent closed the connection"
        );
        assert_eq!(
            AcpError::Protocol("bad".to_owned()).to_string(),
            "ACP protocol error: bad"
        );
        assert_eq!(
            AcpError::InvalidCwd(PathBuf::from("relative")).to_string(),
            "the session cwd relative is not absolute"
        );
        assert_eq!(
            AcpError::UnsupportedVersion(ProtocolVersion::LATEST).to_string(),
            "the agent speaks ACP 1, this client speaks 1"
        );
        assert_eq!(
            AcpError::Agent(acp::Error::new(-32603, "boom")).to_string(),
            "the agent refused: boom"
        );
        assert_eq!(
            AcpError::Transport(TransportError::Closed).to_string(),
            "transport is closed"
        );
        assert!(
            AcpError::Transport(TransportError::Closed)
                .source()
                .is_some()
        );
        assert!(AcpError::Protocol("bad".to_owned()).source().is_none());
    }
}

//! The runners: scripted, failing, and ACP.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use mokmok_acp::{
    AcpClient, CancelToken, ConnectionConfig, ContentBlock, Handler, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StdioConfig, StopReason,
    ToolCallStatus, ToolKind, WebSocketConfig,
};
use mokmok_control_core::{
    MAX_PROMPT_BYTES, PromptInput, TaskSpec, clip_to_char_boundary, compose_prompt,
};
use mokmok_journal::JournalEvent;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::supervisor::{Reporter, RunOutcome, Runner};

/// The name pattern `TaskSpec::validate` accepts, so a table cannot name an agent no
/// task may ask for.
const MAX_AGENT_NAME_BYTES: usize = 32;

/// The longest host name [`AgentTable::validate`] accepts, matching the task spec.
const MAX_HOST_BYTES: usize = 64;

/// How often the watcher checks whether a human cancelled the run.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// The buffered text length that makes a streamed message or thought durable.
const FLUSH_BYTES: usize = 8 * 1024;

/// The longest assistant text the completed summary carries.
const SUMMARY_BYTES: usize = 4 * 1024;

/// The summary used when the agent finishes without having streamed any text.
const NO_SUMMARY: &str = "the agent finished without a summary";

/// The title used for a tool call update that changes only the status.
const UNTITLED_TOOL_CALL: &str = "tool call";

/// Why a task cannot run where it says it runs.
const NO_WORKSPACE: &str = "the task has no workspace";
const RELATIVE_WORKSPACE: &str = "the task workspace must be an absolute path";
const MISSING_WORKSPACE: &str = "the task workspace does not exist";
const WORKSPACE_NOT_DIRECTORY: &str = "the task workspace is not a directory";
const WORKSPACE_OUTSIDE_ROOT: &str = "the task workspace is outside the allowed root";
const BAD_WORKSPACE_ROOT: &str = "the workspace root does not exist";

/// Runs a fixed transcript and completes at once, for tests and the demo mode.
#[derive(Clone, Debug)]
pub struct ScriptedRunner {
    events: Vec<JournalEvent>,
    summary: String,
}

impl ScriptedRunner {
    #[must_use]
    pub fn new(
        events: Vec<JournalEvent>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            events,
            summary: summary.into(),
        }
    }
}

impl Runner for ScriptedRunner {
    fn run(
        &self,
        _spec: &TaskSpec,
        reporter: &dyn Reporter,
    ) -> RunOutcome {
        let _ = reporter.started("scripted-session");
        for event in &self.events {
            let _ = reporter.update(event.clone());
        }
        RunOutcome::Completed {
            summary: self.summary.clone(),
        }
    }
}

/// Fails every task with a fixed reason.
#[derive(Clone, Debug)]
pub struct FailingRunner {
    reason: String,
}

impl FailingRunner {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Runner for FailingRunner {
    fn run(
        &self,
        _spec: &TaskSpec,
        _reporter: &dyn Reporter,
    ) -> RunOutcome {
        RunOutcome::Failed {
            reason: self.reason.clone(),
        }
    }
}

/// How a table entry reaches its agent.
///
/// The JSON form is `{"transport":"stdio","program":…,"args":[…]}` or
/// `{"transport":"websocket","url":…,"secret":…,"bearer":…}`. The two credentials are
/// optional and become the `server-key` query parameter and the `Authorization: Bearer`
/// header of the opening handshake.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum AgentTransport {
    Stdio {
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
    },
    #[serde(rename = "websocket")]
    WebSocket {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer: Option<String>,
    },
}

impl fmt::Debug for AgentTransport {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Stdio { program, args } => formatter
                .debug_struct("Stdio")
                .field("program", program)
                .field("args", args)
                .finish(),
            Self::WebSocket {
                url,
                secret,
                bearer,
            } => formatter
                .debug_struct("WebSocket")
                .field("url", &redacted_url(url))
                .field("secret", &secret.as_ref().map(|_| "<redacted>"))
                .field("bearer", &bearer.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// One configured agent.
#[derive(Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AgentConfig {
    pub transport: AgentTransport,
}

impl fmt::Debug for AgentConfig {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter
            .debug_tuple("AgentConfig")
            .field(&self.transport)
            .finish()
    }
}

/// The agents a runner may start, keyed by the name a task carries.
///
/// `hosts` maps a task's `host` value to a transport override, so a task can name a
/// remote endpoint the agent table does not pin.
#[derive(Clone, Deserialize, Serialize)]
pub struct AgentTable {
    pub agents: BTreeMap<String, AgentConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<String, AgentTransport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl fmt::Debug for AgentTable {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter
            .debug_struct("AgentTable")
            .field("agents", &self.agents)
            .field("hosts", &self.hosts)
            .field("default", &self.default)
            .finish()
    }
}

// A URL may carry the `server-key` query, so only the scheme, authority, and path are
// ever printed.
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

impl AgentTable {
    /// Parses and validates a table.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Json`] for malformed JSON, and every error
    /// [`AgentTable::validate`] reports.
    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let table: Self =
            serde_json::from_str(text).map_err(|error| ConfigError::Json(error.to_string()))?;
        table.validate()?;
        Ok(table)
    }

    /// Every agent name, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.agents.keys().map(String::as_str).collect()
    }

    /// The name a task with an empty agent field resolves to.
    ///
    /// That is the configured default, or the only agent when the table holds one.
    #[must_use]
    pub fn default_name(&self) -> Option<&str> {
        if let Some(default) = &self.default {
            return Some(default);
        }
        if self.agents.len() == 1 {
            return self.agents.keys().next().map(String::as_str);
        }
        None
    }

    /// The config a task's agent name names, or the default for an empty name.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NoDefaultAgent`] for an empty name when the table has no
    /// default, and [`ConfigError::UnknownAgent`] when the resolved name is not in the
    /// table.
    pub fn resolve(
        &self,
        name: &str,
    ) -> Result<&AgentConfig, ConfigError> {
        let name = if name.is_empty() {
            self.default_name().ok_or(ConfigError::NoDefaultAgent)?
        } else {
            name
        };
        self.agents
            .get(name)
            .ok_or_else(|| ConfigError::UnknownAgent(name.to_owned()))
    }

    /// Checks every name, program, URL, host, and the default against the table's rules.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NoAgents`] for an empty table, and the variant naming the
    /// first entry that breaks a rule.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.agents.is_empty() {
            return Err(ConfigError::NoAgents);
        }
        for (name, config) in &self.agents {
            if !valid_agent_name(name) {
                return Err(ConfigError::InvalidName(name.clone()));
            }
            validate_transport(name, &config.transport)?;
        }
        for (host, transport) in &self.hosts {
            if !valid_host_name(host) {
                return Err(ConfigError::InvalidHost(host.clone()));
            }
            validate_transport(host, transport)?;
        }
        if let Some(default) = &self.default
            && !self.agents.contains_key(default)
        {
            return Err(ConfigError::UnknownAgent(default.clone()));
        }
        Ok(())
    }
}

fn validate_transport(
    name: &str,
    transport: &AgentTransport,
) -> Result<(), ConfigError> {
    match transport {
        AgentTransport::Stdio { program, .. } => {
            if !program.is_absolute() {
                return Err(ConfigError::RelativeProgram {
                    agent: name.to_owned(),
                    program: program.display().to_string(),
                });
            }
        },
        AgentTransport::WebSocket { url, .. } => {
            // The URL parser normalises `wss:/host` to `wss://host`, so the raw prefix
            // is what rejects a half-written URL.
            let lower = url.to_ascii_lowercase();
            let Ok(parsed) = Url::parse(url) else {
                return Err(ConfigError::InvalidUrl {
                    agent: name.to_owned(),
                });
            };
            if !(lower.starts_with("wss://") || lower.starts_with("ws://"))
                || parsed.host().is_none()
            {
                return Err(ConfigError::InvalidUrl {
                    agent: name.to_owned(),
                });
            }
            match parsed.scheme() {
                "wss" => {},
                "ws" if is_loopback(&parsed) => {},
                "ws" => {
                    return Err(ConfigError::InsecureUrl {
                        agent: name.to_owned(),
                    });
                },
                _ => {
                    return Err(ConfigError::InvalidUrl {
                        agent: name.to_owned(),
                    });
                },
            }
        },
    }
    Ok(())
}

// `ws://` travels in cleartext, so only a loopback endpoint may use it.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn valid_agent_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() <= MAX_AGENT_NAME_BYTES
        && bytes.first().is_some_and(|byte| is_lower_alnum(*byte))
        && bytes
            .iter()
            .all(|byte| is_lower_alnum(*byte) || *byte == b'-')
}

fn valid_host_name(host: &str) -> bool {
    let bytes = host.as_bytes();
    bytes.len() <= MAX_HOST_BYTES
        && bytes.first().is_some_and(|byte| is_lower_alnum(*byte))
        && bytes
            .iter()
            .all(|byte| is_lower_alnum(*byte) || *byte == b'.' || *byte == b'-')
}

const fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Why a table could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Json(String),
    NoAgents,
    InvalidName(String),
    InvalidHost(String),
    RelativeProgram { agent: String, program: String },
    InvalidUrl { agent: String },
    InsecureUrl { agent: String },
    UnknownAgent(String),
    UnknownHost(String),
    NoDefaultAgent,
}

impl fmt::Display for ConfigError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Json(message) => write!(formatter, "invalid agent table JSON: {message}"),
            Self::NoAgents => formatter.write_str("the agent table is empty"),
            Self::InvalidName(name) => write!(
                formatter,
                "agent name {name:?} must match [a-z0-9][a-z0-9-]{{0,31}}"
            ),
            Self::InvalidHost(host) => write!(
                formatter,
                "host {host:?} must match [a-z0-9][a-z0-9.-]{{0,63}}"
            ),
            Self::RelativeProgram { agent, program } => {
                write!(
                    formatter,
                    "agent {agent:?} program {program:?} must be absolute"
                )
            },
            Self::InvalidUrl { agent } => write!(
                formatter,
                "agent {agent:?} url must start with wss:// (or ws:// on loopback)"
            ),
            Self::InsecureUrl { agent } => write!(
                formatter,
                "agent {agent:?} url must use wss:// unless the host is loopback"
            ),
            Self::UnknownAgent(name) => write!(formatter, "no agent is named {name:?}"),
            Self::UnknownHost(host) => write!(formatter, "no host is named {host:?}"),
            Self::NoDefaultAgent => formatter.write_str("the table has no default agent"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Runs tasks on the ACP agents the table names.
pub struct AcpRunner {
    table: AgentTable,
    timeout: Duration,
    workspace_root: Option<PathBuf>,
}

impl AcpRunner {
    #[must_use]
    pub const fn new(
        table: AgentTable,
        timeout: Duration,
    ) -> Self {
        Self {
            table,
            timeout,
            workspace_root: None,
        }
    }

    /// Confines every run to a workspace under `root`.
    #[must_use]
    pub fn with_workspace_root(
        mut self,
        root: impl Into<PathBuf>,
    ) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    #[must_use]
    pub const fn table(&self) -> &AgentTable {
        &self.table
    }

    // A task's `host`, when it names one, replaces the agent entry's transport; a host
    // the table does not name is an error rather than a silent fallback.
    fn route(
        &self,
        spec: &TaskSpec,
    ) -> Result<ConnectionConfig, String> {
        let config = self
            .table
            .resolve(&spec.agent)
            .map_err(|error| error.to_string())?;
        let transport = match spec.host.as_deref() {
            Some(host) => self
                .table
                .hosts
                .get(host)
                .ok_or_else(|| ConfigError::UnknownHost(host.to_owned()).to_string())?,
            None => &config.transport,
        };
        Ok(connection(transport))
    }
}

impl Runner for AcpRunner {
    fn run(
        &self,
        spec: &TaskSpec,
        reporter: &dyn Reporter,
    ) -> RunOutcome {
        let config = match self.route(spec) {
            Ok(config) => config,
            Err(reason) => return RunOutcome::Failed { reason },
        };
        if reporter.cancelled() {
            return RunOutcome::Cancelled;
        }
        let cwd = match resolve_workspace(spec.workspace.as_deref(), self.workspace_root.as_deref())
        {
            Ok(path) => path,
            Err(reason) => return RunOutcome::Failed { reason },
        };
        let prompt = compose_prompt(
            &PromptInput {
                title: &spec.title,
                instruction: &spec.instruction,
                workspace: spec.workspace.as_deref(),
            },
            MAX_PROMPT_BYTES,
        );
        let mut client = match AcpClient::open_with_timeout(config, self.timeout) {
            Ok(client) => client,
            Err(error) => {
                return RunOutcome::Failed {
                    reason: error.to_string(),
                };
            },
        };
        let session = match client.new_session(&cwd) {
            Ok(session) => session,
            Err(error) => {
                return RunOutcome::Failed {
                    reason: error.to_string(),
                };
            },
        };
        if let Err(reason) = reporter.started(&session.id().to_string()) {
            return RunOutcome::Failed { reason };
        }
        let cancel = CancelToken::new();
        // A quiet agent never calls the handler again, so the human's cancel has to be
        // noticed off the message pump or the turn waits out its full timeout.
        let (turn, last_message) = thread::scope(|scope| {
            let (stop, stopped) = mpsc::channel::<()>();
            let watcher_cancel = cancel.clone();
            scope.spawn(move || {
                loop {
                    if reporter.cancelled() {
                        watcher_cancel.cancel();
                        return;
                    }
                    match stopped.recv_timeout(CANCEL_POLL) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => {},
                    }
                }
            });
            let mut handler = AcpHandler {
                reporter,
                cancel: cancel.clone(),
                message: String::new(),
                thought: String::new(),
                final_message: String::new(),
                last_message: None,
            };
            let turn = client.delegate_with(&session, &prompt, &mut handler, &cancel);
            // A failed turn still owns whatever the agent streamed before it broke, so
            // the transcript keeps the partial output.
            handler.finish();
            drop(stop);
            (turn, handler.last_message)
        });
        match turn {
            Ok(turn) => match turn.stop_reason {
                StopReason::EndTurn => RunOutcome::Completed {
                    summary: last_message.unwrap_or_else(|| NO_SUMMARY.to_owned()),
                },
                StopReason::Cancelled => RunOutcome::Cancelled,
                StopReason::MaxTokens => RunOutcome::Failed {
                    reason: "the agent reached its token limit".to_owned(),
                },
                StopReason::MaxTurnRequests => RunOutcome::Failed {
                    reason: "the agent reached its turn limit".to_owned(),
                },
                StopReason::Refusal => RunOutcome::Failed {
                    reason: "the agent refused the instruction".to_owned(),
                },
                other => RunOutcome::Failed {
                    reason: format!("the agent stopped for an unknown reason: {other:?}"),
                },
            },
            Err(error) => RunOutcome::Failed {
                reason: error.to_string(),
            },
        }
    }
}

fn connection(transport: &AgentTransport) -> ConnectionConfig {
    match transport {
        AgentTransport::Stdio { program, args } => {
            ConnectionConfig::Stdio(StdioConfig::new(program).args(args))
        },
        AgentTransport::WebSocket {
            url,
            secret,
            bearer,
        } => {
            let mut config = WebSocketConfig::new(url);
            if let Some(secret) = secret {
                config = config.server_key(secret);
            }
            if let Some(bearer) = bearer {
                config = config.bearer(bearer);
            }
            ConnectionConfig::WebSocket(config)
        },
    }
}

// A coding agent works in a real directory on this host, so the workspace is resolved
// before the session exists and never silently falls back to the daemon's cwd.
fn resolve_workspace(
    workspace: Option<&str>,
    root: Option<&Path>,
) -> Result<PathBuf, String> {
    let Some(raw) = workspace else {
        return Err(NO_WORKSPACE.to_owned());
    };
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(RELATIVE_WORKSPACE.to_owned());
    }
    let path = std::fs::canonicalize(path).map_err(|_| MISSING_WORKSPACE.to_owned())?;
    if !path.is_dir() {
        return Err(WORKSPACE_NOT_DIRECTORY.to_owned());
    }
    if let Some(root) = root {
        let root = std::fs::canonicalize(root).map_err(|_| BAD_WORKSPACE_ROOT.to_owned())?;
        if !path.starts_with(&root) {
            return Err(WORKSPACE_OUTSIDE_ROOT.to_owned());
        }
    }
    Ok(path)
}

struct AcpHandler<'a> {
    reporter: &'a dyn Reporter,
    cancel: CancelToken,
    message: String,
    thought: String,
    // The contiguous assistant message run, cleared at a thought or tool-call
    // boundary.
    final_message: String,
    // The best available summary so far, kept for a run that ends on a thought or a
    // tool call.
    last_message: Option<String>,
}

impl AcpHandler<'_> {
    fn flush_message(&mut self) {
        if self.message.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.message);
        self.last_message = Some(clip_summary(&text));
        let _ = self.reporter.update(JournalEvent::Message { text });
    }

    fn flush_thought(&mut self) {
        if self.thought.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.thought);
        self.last_message = Some(clip_summary(&text));
        let _ = self.reporter.update(JournalEvent::Thought { text });
    }

    fn finish(&mut self) {
        self.flush_message();
        self.flush_thought();
        if !self.final_message.is_empty() {
            self.last_message = Some(clip_summary(&self.final_message));
        }
    }

    fn record_permission(
        &mut self,
        title: String,
        allowed: bool,
    ) {
        let status = if allowed { "allowed" } else { "refused" };
        let _ = self.reporter.update(JournalEvent::Permission {
            title,
            status: status.to_owned(),
        });
    }
}

impl Handler for AcpHandler<'_> {
    fn session_update(
        &mut self,
        notification: SessionNotification,
    ) {
        if self.reporter.cancelled() {
            self.cancel.cancel();
        }
        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                // A thought that is still buffered belongs before this message, so the
                // transcript keeps the order the agent sent.
                self.flush_thought();
                if let Some(text) = text_of(chunk.content) {
                    self.message.push_str(&text);
                    self.final_message.push_str(&text);
                    if self.message.len() >= FLUSH_BYTES {
                        self.flush_message();
                    }
                }
            },
            SessionUpdate::AgentThoughtChunk(chunk) => {
                self.flush_message();
                self.final_message.clear();
                if let Some(text) = text_of(chunk.content) {
                    self.thought.push_str(&text);
                    if self.thought.len() >= FLUSH_BYTES {
                        self.flush_thought();
                    }
                }
            },
            SessionUpdate::ToolCall(call) => {
                self.flush_message();
                self.flush_thought();
                self.final_message.clear();
                let _ = self.reporter.update(JournalEvent::ToolCall {
                    title: call.title,
                    status: tool_status(call.status).to_owned(),
                });
            },
            SessionUpdate::ToolCallUpdate(update) => {
                self.flush_message();
                self.flush_thought();
                self.final_message.clear();
                let title = update.fields.title;
                let status = update.fields.status;
                if title.is_some() || status.is_some() {
                    let _ = self.reporter.update(JournalEvent::ToolCall {
                        title: title.unwrap_or_else(|| UNTITLED_TOOL_CALL.to_owned()),
                        status: status.map_or("pending", tool_status).to_owned(),
                    });
                }
            },
            _ => {},
        }
    }

    fn request_permission(
        &mut self,
        request: RequestPermissionRequest,
    ) -> RequestPermissionResponse {
        let fields = request.tool_call.fields;
        let title = fields
            .title
            .unwrap_or_else(|| UNTITLED_TOOL_CALL.to_owned());
        // The buffered text was streamed before this request, so the transcript has to
        // show it first even though the buffer would otherwise flush later.
        self.flush_message();
        self.flush_thought();
        let read_only = matches!(
            fields.kind,
            Some(ToolKind::Read | ToolKind::Search | ToolKind::Think)
        );
        if !read_only {
            self.record_permission(title, false);
            return RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        }
        let options = &request.options;
        let chosen = options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .or_else(|| {
                options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowAlways)
            });
        let Some(option) = chosen else {
            self.record_permission(title, false);
            return RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        };
        self.record_permission(title, true);
        RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(option.option_id.clone()),
        ))
    }
}

fn text_of(content: ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text),
        _ => None,
    }
}

// The flushed transcript keeps every byte; only the Completed payload is clipped.
fn clip_summary(text: &str) -> String {
    clip_to_char_boundary(text, SUMMARY_BYTES).to_owned()
}

const fn tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct Capture {
        events: Mutex<Vec<JournalEvent>>,
    }

    impl Reporter for Capture {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            event: JournalEvent,
        ) -> Result<(), String> {
            self.events
                .lock()
                .expect("the capture lock is not poisoned")
                .push(event);
            Ok(())
        }

        fn cancelled(&self) -> bool {
            false
        }
    }

    fn handler(capture: &Capture) -> AcpHandler<'_> {
        AcpHandler {
            reporter: capture,
            cancel: CancelToken::new(),
            message: String::new(),
            thought: String::new(),
            final_message: String::new(),
            last_message: None,
        }
    }

    fn notification(update: &serde_json::Value) -> SessionNotification {
        serde_json::from_value(json!({"sessionId": "session-1", "update": update}))
            .expect("the notification decodes")
    }

    fn text(
        session_update: &str,
        text: &str,
    ) -> SessionNotification {
        notification(&json!({
            "sessionUpdate": session_update,
            "content": {"type": "text", "text": text},
        }))
    }

    fn tool_call(
        title: &str,
        status: &str,
    ) -> SessionNotification {
        notification(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "title": title,
            "status": status,
        }))
    }

    fn empty_tool_update() -> SessionNotification {
        notification(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "call-1"}))
    }

    fn permission(
        kind: Option<&str>,
        options: &serde_json::Value,
    ) -> RequestPermissionRequest {
        let mut tool_call = json!({"toolCallId": "call-1", "title": "read a file"});
        if let Some(kind) = kind {
            tool_call["kind"] = json!(kind);
        }
        serde_json::from_value(json!({
            "sessionId": "session-1",
            "toolCall": tool_call,
            "options": options,
        }))
        .expect("the permission request decodes")
    }

    fn allow_once() -> serde_json::Value {
        json!([{"optionId": "allow", "name": "Allow", "kind": "allow_once"}])
    }

    #[test]
    fn an_allowed_permission_keeps_the_buffered_text_first() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_message_chunk", "answering"));
        let response = handler.request_permission(permission(Some("read"), &allow_once()));
        handler.finish();

        assert!(matches!(
            response.outcome,
            RequestPermissionOutcome::Selected(_)
        ));
        assert_eq!(
            events(&capture),
            vec![
                JournalEvent::Message {
                    text: "answering".to_owned()
                },
                JournalEvent::Permission {
                    title: "read a file".to_owned(),
                    status: "allowed".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_refused_permission_is_recorded_too() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let response = handler.request_permission(permission(Some("edit"), &allow_once()));
        handler.finish();

        assert_eq!(response.outcome, RequestPermissionOutcome::Cancelled);
        assert_eq!(
            events(&capture),
            vec![JournalEvent::Permission {
                title: "read a file".to_owned(),
                status: "refused".to_owned()
            }]
        );
    }

    fn events(capture: &Capture) -> Vec<JournalEvent> {
        capture
            .events
            .lock()
            .expect("the capture lock is not poisoned")
            .clone()
    }

    fn messages(capture: &Capture) -> Vec<String> {
        events(capture)
            .into_iter()
            .filter_map(|event| match event {
                JournalEvent::Message { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_thought_before_a_message_keeps_the_streamed_order() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_thought_chunk", "thinking"));
        handler.session_update(text("agent_message_chunk", "answering"));
        handler.session_update(tool_call("read", "pending"));
        handler.finish();
        assert_eq!(
            events(&capture),
            vec![
                JournalEvent::Thought {
                    text: "thinking".to_owned()
                },
                JournalEvent::Message {
                    text: "answering".to_owned()
                },
                JournalEvent::ToolCall {
                    title: "read".to_owned(),
                    status: "pending".to_owned()
                },
            ]
        );
        assert_eq!(handler.last_message.as_deref(), Some("answering"));
    }

    #[test]
    fn the_summary_is_the_final_message_run() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_message_chunk", "first answer"));
        handler.session_update(text("agent_thought_chunk", "a thought"));
        handler.session_update(text("agent_message_chunk", "second answer"));
        handler.finish();
        assert_eq!(handler.last_message.as_deref(), Some("second answer"));
    }

    #[test]
    fn a_turn_that_ends_on_a_thought_summarizes_the_thought() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_thought_chunk", "a thought"));
        handler.finish();
        assert_eq!(handler.last_message.as_deref(), Some("a thought"));
    }

    #[test]
    fn a_tool_call_without_fields_is_still_a_boundary() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_message_chunk", "answer"));
        handler.session_update(empty_tool_update());
        handler.session_update(text("agent_message_chunk", "more"));
        handler.finish();
        assert_eq!(messages(&capture), vec!["answer", "more"]);
        assert_eq!(handler.last_message.as_deref(), Some("more"));
    }

    #[test]
    fn an_empty_chunk_records_nothing() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        handler.session_update(text("agent_message_chunk", ""));
        handler.session_update(text("agent_thought_chunk", ""));
        handler.finish();
        assert_eq!(events(&capture), Vec::<JournalEvent>::new());
        assert_eq!(handler.last_message, None);
    }

    #[test]
    fn the_message_buffer_flushes_at_its_size() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let exact = "a".repeat(FLUSH_BYTES);
        handler.session_update(text("agent_message_chunk", &exact));
        assert_eq!(messages(&capture), vec![exact]);
    }

    #[test]
    fn a_chunk_past_the_message_budget_flushes_too() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let over = "a".repeat(FLUSH_BYTES + 1);
        handler.session_update(text("agent_message_chunk", &over));
        assert_eq!(messages(&capture), vec![over]);
    }

    #[test]
    fn the_thought_buffer_flushes_at_its_size() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let exact = "t".repeat(FLUSH_BYTES);
        handler.session_update(text("agent_thought_chunk", &exact));
        assert_eq!(
            events(&capture),
            vec![JournalEvent::Thought { text: exact }]
        );
    }

    #[test]
    fn the_summary_spans_every_flushed_chunk_of_the_final_run() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let first = "a".repeat(FLUSH_BYTES);
        let second = "b".repeat(FLUSH_BYTES);
        let third = "c".repeat(100);
        handler.session_update(text("agent_message_chunk", &first));
        handler.session_update(text("agent_message_chunk", &second));
        handler.session_update(text("agent_message_chunk", &third));
        handler.finish();
        assert_eq!(
            messages(&capture).concat(),
            format!("{first}{second}{third}")
        );
        let expected = &format!("{first}{second}")[..SUMMARY_BYTES];
        assert_eq!(handler.last_message.as_deref(), Some(expected));
    }

    #[test]
    fn the_summary_is_clipped_but_the_transcript_is_not() {
        let capture = Capture::default();
        let mut handler = handler(&capture);
        let long = "b".repeat(SUMMARY_BYTES + 100);
        handler.session_update(text("agent_message_chunk", &long));
        handler.finish();
        assert_eq!(messages(&capture), vec![long]);
        assert_eq!(
            handler.last_message.as_deref().map(str::len),
            Some(SUMMARY_BYTES)
        );
    }

    #[test]
    fn the_buffers_are_the_documented_sizes() {
        assert_eq!(FLUSH_BYTES, 8 * 1024);
        assert_eq!(SUMMARY_BYTES, 4 * 1024);
    }
}

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
use mokmok_control_core::{AgentUpdate, MAX_PROMPT_BYTES, PromptInput, TaskSpec, compose_prompt};
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
    updates: Vec<AgentUpdate>,
    summary: String,
}

impl ScriptedRunner {
    #[must_use]
    pub fn new(
        updates: Vec<AgentUpdate>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            updates,
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
        for update in &self.updates {
            let _ = reporter.update(update.clone());
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
                last_message: None,
            };
            let turn = client.delegate_with(&session, &prompt, &mut handler, &cancel);
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
    last_message: Option<String>,
}

impl Handler for AcpHandler<'_> {
    fn session_update(
        &mut self,
        notification: SessionNotification,
    ) {
        if self.reporter.cancelled() {
            self.cancel.cancel();
        }
        let recorded = match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => text_of(chunk.content).map(|text| {
                self.last_message = Some(text.clone());
                AgentUpdate::Message { text }
            }),
            SessionUpdate::AgentThoughtChunk(chunk) => {
                text_of(chunk.content).map(|text| AgentUpdate::Thought { text })
            },
            SessionUpdate::ToolCall(call) => Some(AgentUpdate::ToolCall {
                title: call.title,
                status: tool_status(call.status).to_owned(),
            }),
            SessionUpdate::ToolCallUpdate(update) => {
                let title = update.fields.title;
                let status = update.fields.status;
                if title.is_none() && status.is_none() {
                    None
                } else {
                    Some(AgentUpdate::ToolCall {
                        title: title.unwrap_or_else(|| UNTITLED_TOOL_CALL.to_owned()),
                        status: status.map_or("pending", tool_status).to_owned(),
                    })
                }
            },
            _ => None,
        };
        if let Some(update) = recorded {
            let _ = self.reporter.update(update);
        }
    }

    fn request_permission(
        &mut self,
        request: RequestPermissionRequest,
    ) -> RequestPermissionResponse {
        let read_only = matches!(
            request.tool_call.fields.kind,
            Some(ToolKind::Read | ToolKind::Search | ToolKind::Think)
        );
        if !read_only {
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
        chosen.map_or_else(
            || RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            |option| {
                RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                    SelectedPermissionOutcome::new(option.option_id.clone()),
                ))
            },
        )
    }
}

fn text_of(content: ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text),
        _ => None,
    }
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

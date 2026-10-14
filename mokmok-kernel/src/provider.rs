//! The real provider: an OpenAI-compatible chat-completions call over TLS.
//!
//! The provider is trusted configuration rather than a capability, so its destination is validated
//! through [`mokmok_egress`] before any socket exists and the validated addresses are pinned into
//! the HTTP client. Nothing of the wire crosses the runtime's [`Provider`] port: the request is
//! built here, the answer is parsed back into the runtime's [`Answer`], and the credential lives in
//! a value whose [`Debug`] prints `REDACTED`.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use mokmok_agent_runtime::{
    Action, Answer, ArtifactRef, Context, ContextAct, ContextTurn, Error as AgentError,
    MAX_PROSE_BYTES, Provider, Usage,
};
use mokmok_artifact::{BlobStore, ContentHash};
use mokmok_egress::{
    AddressClass, AuditRecord, AuditSink, CallContext, DecisionName, Destination, Host,
    PolicyClient, Port, Refusal, Resolver, Rules, Scheme, Validated,
};
use mokmok_observability::{AuditEntry, AuditKind, AuditLog, Journal as AuditJournal, Outcome};
use mokmok_protocol::{ExecutionID, SessionID};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use uuid::Uuid;

use crate::authority::AGENT;
use crate::config::Config;

/// The one tool the request offers, matching the runtime's tool surface.
const TOOL_NAME: &str = "run_shell";

/// The tool description the endpoint is handed.
const TOOL_DESCRIPTION: &str = "Run one shell command with `sh -c` in a fresh, empty working \
     directory. The checkout is reached by absolute path, stdin is empty, and there is no TCP \
     network.";

/// The chat-completions path appended to a configured endpoint.
const COMPLETIONS: &str = "/chat/completions";

/// The audit journal the destination decision lands in, inside the data directory.
const AUDIT_FILE: &str = "audit.jsonl";

/// The port an `https` URL uses when it names none.
const DEFAULT_HTTPS_PORT: u16 = 443;

/// The port an `http` URL uses when it names none.
const DEFAULT_HTTP_PORT: u16 = 80;

/// How long one endpoint call may run before the client abandons it.
///
/// An endpoint that accepts a request and never answers would otherwise hold a step forever. The
/// runtime re-asks an interrupted model call under a new attempt with byte-identical context, so an
/// abandoned call spends one of the turn's attempts rather than the session. The bound is wide
/// enough for a long answer and short enough that the three attempts a turn gets fit inside a wait
/// a human will sit through.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the provider could not be built or one call could not complete.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The credential file could not be read.
    #[error("the provider credential file {path} could not be read")]
    Credential {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The credential file held no value.
    #[error("the provider credential file {path} is empty")]
    CredentialEmpty { path: PathBuf },
    /// No credential file was named.
    #[error("no --provider-credential-file is named, so the provider has no credential")]
    NoCredential,
    /// No model id was named.
    #[error("no --model is named, so the provider has nothing to ask")]
    NoModel,
    /// The endpoint was neither an `http` nor an `https` URL.
    #[error("the endpoint {endpoint} is not an http or https URL")]
    EndpointScheme { endpoint: String },
    /// The endpoint was cleartext HTTP to a host that is not loopback.
    #[error(
        "the endpoint {endpoint} is cleartext http, which is allowed only for loopback \
         (127.0.0.1, ::1, or localhost)"
    )]
    EndpointCleartext { endpoint: String },
    /// The endpoint was not `host[:port][/path]`.
    #[error("the endpoint {endpoint} is not a usable URL: {reason}")]
    EndpointForm {
        endpoint: String,
        reason: &'static str,
    },
    /// The endpoint host could not be resolved.
    #[error("the endpoint host {host} could not be resolved")]
    Resolve {
        host: String,
        #[source]
        source: io::Error,
    },
    /// The destination was refused before any socket existed.
    #[error("the provider destination was refused: {0}")]
    Destination(#[from] Refusal),
    /// The audit journal could not be opened.
    #[error("the provider audit journal could not be opened")]
    Audit(#[source] mokmok_observability::Error),
    /// The HTTP client could not be built.
    #[error("the provider HTTP client could not be built")]
    Client(#[source] reqwest::Error),
    /// The request body could not be encoded.
    #[error("the provider request body could not be encoded")]
    Encode(#[source] serde_json::Error),
    /// The request could not be sent or the response body could not be read.
    #[error("the provider call failed in transport")]
    Transport(#[source] reqwest::Error),
    /// The endpoint answered a status that is not 2xx.
    #[error("the endpoint answered http status {status}")]
    Status { status: u16 },
    /// The answer body was not JSON.
    #[error("the endpoint answered a body that is not json")]
    Json(#[source] serde_json::Error),
    /// The answer body held no choice.
    #[error("the endpoint answered no choices")]
    NoChoices,
    /// The answer text could not be stored.
    #[error("the provider could not store the answer text")]
    Artifact(#[source] mokmok_artifact::Error),
}

/// The provider credential, read from the file `--provider-credential-file` names.
///
/// The value is deliberately not a [`Config`] field and never enters the process environment, so
/// nothing the daemon logs or a child inherits can carry it. Its [`Debug`] prints `REDACTED`.
pub struct Credential(String);

impl Credential {
    /// Reads the credential from `path`, refusing a missing or empty file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Credential`] naming the path when the file cannot be read, and
    /// [`Error::CredentialEmpty`] naming the path when it holds nothing but whitespace.
    pub fn read(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Credential {
            path: path.to_path_buf(),
            source,
        })?;
        let secret = text.trim();

        if secret.is_empty() {
            return Err(Error::CredentialEmpty {
                path: path.to_path_buf(),
            });
        }

        Ok(Self(secret.to_owned()))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str("REDACTED")
    }
}

/// An endpoint parsed by hand from the `--endpoint` value.
#[derive(Debug)]
pub struct Endpoint {
    /// The host to look up, exactly as the URL wrote it, with an IPv6 literal's brackets removed.
    pub host: String,
    /// The port, never zero.
    pub port: Port,
    /// The base path, with any trailing slash removed and no leading slash when it is empty.
    pub path: String,
    scheme: Scheme,
}

impl Endpoint {
    /// The transport this endpoint's port speaks.
    #[must_use]
    pub const fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The classes of address this endpoint's destination may reach.
    ///
    /// A loopback cleartext endpoint admits loopback so a local server can be reached; every other
    /// endpoint stays public-only, which is what keeps a credential from crossing to a private or
    /// link-local address.
    #[must_use]
    pub const fn rules(&self) -> Rules {
        match self.scheme {
            Scheme::Https => Rules::public_only(),
            Scheme::Http => Rules::public_only().permit(AddressClass::Loopback),
        }
    }

    /// The `host:port` authority a URL writes, bracketing an IPv6 literal.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{}]:{}", self.host, self.port.get())
        } else {
            format!("{}:{}", self.host, self.port.get())
        }
    }

    /// The chat-completions path this endpoint names.
    fn request_path(&self) -> String {
        format!("{}{COMPLETIONS}", self.path)
    }
}

/// Parses an `http://` or `https://host[:port][/path]` endpoint into its host, port, and base path.
///
/// # Errors
///
/// Returns [`Error::EndpointScheme`] for a URL that is neither http nor https,
/// [`Error::EndpointCleartext`] for a cleartext http URL whose host is not loopback, and
/// [`Error::EndpointForm`] for a URL with no host or a port that is not a number in `1..=65535`.
pub fn parse_endpoint(url: &str) -> Result<Endpoint, Error> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        return Err(Error::EndpointScheme {
            endpoint: url.to_owned(),
        });
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, tail.trim_end_matches('/')),
        None => (rest, ""),
    };
    let path = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    let (host, port) = authority_parts(authority, url, scheme)?;

    if scheme == Scheme::Http && !is_loopback_host(&host) {
        return Err(Error::EndpointCleartext {
            endpoint: url.to_owned(),
        });
    }

    Ok(Endpoint {
        host,
        port,
        path,
        scheme,
    })
}

/// Whether `host` names the loopback interface as a literal or as `localhost`.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Splits `host[:port]` into its two parts, defaulting the port.
fn authority_parts(
    authority: &str,
    url: &str,
    scheme: Scheme,
) -> Result<(String, Port), Error> {
    let default = default_port(scheme);
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return Err(form(url, "the endpoint's address literal is not closed"));
        };
        let port = tail
            .strip_prefix(':')
            .map_or_else(|| port_of(default, url), |port| parse_port(port, url));

        (host, port?)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, parse_port(port, url)?),
            None => (authority, port_of(default, url)?),
        }
    };

    if host.is_empty() {
        return Err(form(url, "the endpoint names no host"));
    }

    Ok((host.to_owned(), port))
}

/// The port an endpoint of `scheme` uses when its URL names none.
const fn default_port(scheme: Scheme) -> u16 {
    match scheme {
        Scheme::Https => DEFAULT_HTTPS_PORT,
        Scheme::Http => DEFAULT_HTTP_PORT,
    }
}

/// Reads a numeric port that is not zero.
fn parse_port(
    value: &str,
    url: &str,
) -> Result<Port, Error> {
    let port = value
        .parse::<u16>()
        .map_err(|_| form(url, "the port is not a number in 1..=65535"))?;

    port_of(port, url)
}

/// Wraps a port value, refusing zero.
fn port_of(
    value: u16,
    url: &str,
) -> Result<Port, Error> {
    Port::new(value).ok_or_else(|| form(url, "the port is zero"))
}

/// An endpoint-form refusal with its reason.
fn form(
    url: &str,
    reason: &'static str,
) -> Error {
    Error::EndpointForm {
        endpoint: url.to_owned(),
        reason,
    }
}

/// The host the endpoint named, as the egress crate names it.
fn host_of(name: &str) -> Host {
    name.parse::<IpAddr>()
        .map_or_else(|_| Host::Name(name.to_owned()), Host::Ip)
}

/// The resolver the endpoint's host is looked up with.
struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, io::Error> {
        (host, port)
            .to_socket_addrs()
            .map(std::iter::Iterator::collect)
    }
}

/// The policy port: only the configured host and port may be reached.
struct Permit {
    host: Host,
    port: Port,
}

impl PolicyClient for Permit {
    fn check(
        &self,
        destination: &Destination,
        _: &CallContext,
    ) -> Result<(), Refusal> {
        if destination.host == self.host && destination.port == self.port {
            Ok(())
        } else {
            Err(Refusal::Denied)
        }
    }
}

/// The audit sink: the egress decision becomes one line of the observability audit journal.
struct AuditToLog {
    log: Mutex<AuditJournal>,
}

impl AuditToLog {
    fn open(path: &Path) -> Result<Self, Error> {
        let log = AuditJournal::open(path).map_err(Error::Audit)?;

        Ok(Self {
            log: Mutex::new(log),
        })
    }
}

impl AuditSink for AuditToLog {
    fn record(
        &self,
        record: AuditRecord,
    ) {
        let entry = AuditEntry::new(
            record.at,
            record.principal,
            AuditKind::Egress,
            outcome_of(&record.decision),
        )
        .with_detail(detail_of(&record));
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);

        if let Err(error) = log.append(entry) {
            tracing::warn!(%error, "the provider audit entry did not land");
        }
    }
}

/// The audit outcome an egress decision reads as.
const fn outcome_of(decision: &DecisionName) -> Outcome {
    match decision {
        DecisionName::Allowed => Outcome::Allowed,
        DecisionName::Refused(_) => Outcome::Denied,
        DecisionName::Unreachable => Outcome::Failed,
    }
}

/// The audit detail one egress decision carries.
fn detail_of(record: &AuditRecord) -> String {
    let host = match &record.destination.host {
        Host::Name(name) => name.clone(),
        Host::Ip(address) => address.to_string(),
    };
    let port = record.destination.port.get();

    record.address.map_or_else(
        || format!("model provider {host}:{port}"),
        |address| format!("model provider {host}:{port} at {address}"),
    )
}

/// The audit identity a model call mints, which is not a session fact.
///
/// The destination is validated once, when the provider opens, before any turn exists, so the id
/// is derived from the session rather than from a turn attempt.
fn model_audit_id(session: SessionID) -> Uuid {
    Uuid::new_v5(&session.as_uuid(), b"model")
}

/// The OpenAI-compatible provider, wired to one validated and pinned destination.
pub struct Ollama {
    client: reqwest::Client,
    url: String,
    model: String,
    max_tokens: u64,
    credential: Credential,
    store: BlobStore,
}

impl Ollama {
    /// Builds the provider: reads the credential, resolves and validates the endpoint before any
    /// socket exists, and pins the validated addresses into the HTTP client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoCredential`]/[`Error::NoModel`] when a flag is missing,
    /// [`Error::Credential`]/[`Error::CredentialEmpty`] for the credential file, the errors of
    /// [`parse_endpoint`], [`Error::Resolve`] when the host cannot be resolved,
    /// [`Error::Destination`] when validation refuses the destination, [`Error::Audit`] when the
    /// audit journal cannot be opened, [`Error::Artifact`] when the answer store cannot be opened,
    /// and [`Error::Client`] when the HTTP client cannot be built.
    pub fn open(
        config: &Config,
        session: SessionID,
    ) -> Result<Self, Error> {
        let path = config
            .provider_credential_file
            .as_deref()
            .ok_or(Error::NoCredential)?;
        let credential = Credential::read(path)?;
        let model = config
            .model
            .clone()
            .filter(|model| !model.is_empty())
            .ok_or(Error::NoModel)?;
        let endpoint = parse_endpoint(&config.endpoint)?;
        let host = host_of(&endpoint.host);
        let context = CallContext {
            principal: AGENT,
            execution: ExecutionID::from_uuid(model_audit_id(session)),
        };
        let sink = AuditToLog::open(&config.data_dir.join(AUDIT_FILE))?;
        let addresses = SystemResolver
            .resolve(&endpoint.host, endpoint.port.get())
            .map_err(|source| Error::Resolve {
                host: endpoint.host.clone(),
                source,
            })?;
        let validated = Validated::new(
            Destination {
                scheme: endpoint.scheme(),
                host: host.clone(),
                port: endpoint.port,
            },
            addresses,
            &endpoint.rules(),
            &Permit {
                host,
                port: endpoint.port,
            },
            &context,
            &sink,
        )?;
        let client = reqwest::ClientBuilder::new()
            // A followed redirect would leave the destination this request was validated and
            // pinned for and re-send the goal and the whole transcript to a host mokmok-egress
            // never saw; the pinned address is the only one a redirect may reach.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .resolve_to_addrs(&endpoint.host, validated.addresses())
            .build()
            .map_err(Error::Client)?;

        Ok(Self {
            client,
            url: format!(
                "{}://{}{}",
                scheme_name(endpoint.scheme()),
                endpoint.authority(),
                endpoint.request_path(),
            ),
            model,
            max_tokens: config.max_answer_tokens,
            credential,
            store: BlobStore::open(&config.artifacts).map_err(Error::Artifact)?,
        })
    }
}

/// The URL scheme an endpoint's transport is written as.
const fn scheme_name(scheme: Scheme) -> &'static str {
    match scheme {
        Scheme::Https => "https",
        Scheme::Http => "http",
    }
}

impl Provider for Ollama {
    async fn complete(
        &self,
        context: &Context,
    ) -> Result<Answer, AgentError> {
        let body = serde_json::to_vec(&request_body(context, &self.model, self.max_tokens))
            .map_err(|error| provider_error(Error::Encode(error)))?;
        let response = self
            .client
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .header(
                AUTHORIZATION,
                format!("Bearer {}", self.credential.expose()),
            )
            .body(body)
            .send()
            .await
            .map_err(|error| provider_error(Error::Transport(error)))?;
        let status = response.status();

        if !status.is_success() {
            return Err(provider_error(Error::Status {
                status: status.as_u16(),
            }));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|error| provider_error(Error::Transport(error)))?;
        let answer = parse_answer(&bytes, &self.model).map_err(provider_error)?;
        // The parse names the body by its hash, so storing the same bytes here is what makes the
        // reference the answer record carries resolvable.
        self.store
            .put(&bytes)
            .map_err(|error| provider_error(Error::Artifact(error)))?;

        Ok(answer)
    }
}

/// Erases the adapter's failure behind the runtime's provider port.
fn provider_error(error: Error) -> AgentError {
    AgentError::Provider(Box::new(error))
}

/// The chat-completions body the endpoint was measured to accept.
///
/// The one tool is the runtime's, and its `arguments` are the JSON-string envelope that
/// [`parse_answer`] decodes back. The message list always carries a user message: an endpoint
/// measured here answers a body whose messages are the system prompt alone with
/// `finish_reason: "load"`, no tokens, and no tool call, so the goal must ride the user message
/// even on the first call of a session, before any turn has settled.
#[must_use]
pub fn request_body(
    context: &Context,
    model: &str,
    max_tokens: u64,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": context.system},
            {"role": "user", "content": user_text(context)},
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": TOOL_NAME,
                "description": TOOL_DESCRIPTION,
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"],
                },
            },
        }],
        "max_tokens": max_tokens,
        "stream": false,
    })
}

/// The goal the model was asked to reach, with the settled turns appended once there are any.
fn user_text(context: &Context) -> String {
    let turns = turns_text(context);
    if turns.is_empty() {
        context.goal.clone()
    } else {
        format!("{}\n\n{turns}", context.goal)
    }
}

/// The turns the model may see, as the text the one user message carries after the goal.
fn turns_text(context: &Context) -> String {
    context
        .turns
        .iter()
        .map(turn_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One turn as the text the user message carries, in the labels it has always used.
fn turn_text(turn: &ContextTurn) -> String {
    let mut text = format!("Turn {}:", turn.number.get());
    if let Some(speech) = &turn.speech {
        text.push_str("\nhuman: ");
        text.push_str(speech);
    }
    match &turn.act {
        Some(ContextAct::Ran {
            command,
            observation,
        }) => {
            text.push_str("\ncommand: ");
            text.push_str(command);
            text.push_str("\nobservation: ");
            text.push_str(observation);
        },
        Some(ContextAct::Said { reply }) => {
            text.push_str("\nreply: ");
            text.push_str(reply);
        },
        None => {},
    }

    text
}

/// Decodes the endpoint's answer into the runtime's own vocabulary.
///
/// Only the frozen wire fields are read; the status, the headers, the ids, the timestamps, the
/// fingerprint, `reasoning`, and every unknown key are dropped at this boundary. The whole body is
/// the answer's text, named by its content hash so the journal can read it back.
///
/// # Errors
///
/// Returns [`Error::Json`] when the body is not JSON and [`Error::NoChoices`] when it holds no
/// choice. Nothing else fails: a malformed tool argument, an unfinished answer, and an unexpected
/// `finish_reason` all become [`Action::Malformed`], which is recorded rather than retried.
pub fn parse_answer(
    bytes: &[u8],
    model: &str,
) -> Result<Answer, Error> {
    let body: serde_json::Value = serde_json::from_slice(bytes).map_err(Error::Json)?;
    let choice = body
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or(Error::NoChoices)?;
    let message = choice.get("message").unwrap_or(&serde_json::Value::Null);
    let finish = choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    Ok(Answer {
        action: action_of(finish, message),
        prose: prose_of(message),
        text: reference(bytes),
        model: model.to_owned(),
        usage: Usage {
            input: token(body.get("usage"), "prompt_tokens"),
            output: token(body.get("usage"), "completion_tokens"),
        },
    })
}

/// The model's own words from one message, clipped.
///
/// A message with no string `content` yields nothing rather than an error: the action is what
/// drives the loop, and the prose is for the human.
fn prose_of(message: &serde_json::Value) -> String {
    message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map_or_else(String::new, clip_prose)
}

/// Clips prose to [`MAX_PROSE_BYTES`] on a character boundary.
#[must_use]
pub fn clip_prose(text: &str) -> String {
    if text.len() <= MAX_PROSE_BYTES {
        return text.to_owned();
    }

    let mut end = MAX_PROSE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    text[..end].to_owned()
}

/// The action a `finish_reason` and its message name.
fn action_of(
    finish: &str,
    message: &serde_json::Value,
) -> Action {
    match finish {
        "stop" => Action::Final,
        "tool_calls" => tool_call(message),
        other => Action::Malformed {
            reason: format!("the answer finished with {other:?}"),
        },
    }
}

/// The command a structured tool call names, or why it could not be read.
fn tool_call(message: &serde_json::Value) -> Action {
    let arguments = message
        .get("tool_calls")
        .and_then(|calls| calls.get(0))
        .and_then(|call| call.get("function"))
        .and_then(|function| function.get("arguments"))
        .and_then(serde_json::Value::as_str);
    let Some(arguments) = arguments else {
        return Action::Malformed {
            reason: "the answer named no tool call arguments".to_owned(),
        };
    };
    let decoded: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Action::Malformed {
                reason: format!("the tool arguments are not json: {error}"),
            };
        },
    };

    decoded
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || Action::Malformed {
                reason: "the tool arguments name no command string".to_owned(),
            },
            |command| Action::Command {
                command: command.to_owned(),
            },
        )
}

/// The reference that names `bytes`, which are the whole answer text.
fn reference(bytes: &[u8]) -> ArtifactRef {
    ArtifactRef {
        hash: ContentHash::of(bytes).to_string(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        truncated: false,
    }
}

/// One token count from the usage object, zero when it is absent.
fn token(
    usage: Option<&serde_json::Value>,
    key: &str,
) -> u64 {
    usage
        .and_then(|usage| usage.get(key))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Credential, Error, Ollama, Usage, clip_prose, parse_answer, parse_endpoint,
        request_body,
    };
    use crate::config::{Config, ProviderKind};
    use clap::Parser as _;
    use mokmok_agent_runtime::{Context, ContextAct, ContextTurn, MAX_PROSE_BYTES, Provider as _};
    use mokmok_egress::{AddressClass, Rules, Scheme};
    use mokmok_protocol::SessionID;
    use mokmok_state_machine::TurnNumber;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use uuid::Uuid;

    /// The measured tool-call response, kept as the fixture it is rather than re-encoded.
    const TOOL_CALLS: &str = include_str!("../../.audit/round-2-arena/probe-tool-calls.json");

    /// The measured tool-result response, the same way.
    const TOOL_RESULT: &str = include_str!("../../.audit/round-2-arena/probe-tool-result.json");

    /// A directory this test's own, left behind for the caller to remove.
    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("mokmok-provider-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();

        path
    }

    fn context() -> Context {
        Context {
            system: "the system prompt".to_owned(),
            goal: "the goal".to_owned(),
            turns: Vec::new(),
        }
    }

    #[test]
    fn a_measured_tool_call_decodes_to_the_command() {
        let answer = parse_answer(TOOL_CALLS.as_bytes(), "kimi-k2.7-code").unwrap();

        assert_eq!(
            answer.action,
            Action::Command {
                command: "find . -maxdepth 1 -name \"*.rs\" -type f".to_owned(),
            },
        );
        assert_eq!(answer.model, "kimi-k2.7-code");
        assert_eq!(
            answer.usage,
            Usage {
                input: 93,
                output: 64,
            },
        );
        assert_eq!(answer.text.bytes, u64::try_from(TOOL_CALLS.len()).unwrap());
        assert!(!answer.text.truncated);
    }

    #[test]
    fn a_measured_stop_decodes_to_the_final_answer() {
        let answer = parse_answer(TOOL_RESULT.as_bytes(), "kimi-k2.7-code").unwrap();

        assert_eq!(answer.action, Action::Final);
        assert_eq!(
            answer.usage,
            Usage {
                input: 153,
                output: 55,
            },
        );
    }

    #[test]
    fn tool_arguments_that_are_not_json_are_malformed_rather_than_retried() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{"function": {"name": "run_shell", "arguments": "{not json"}}],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2},
        });

        let answer =
            parse_answer(&serde_json::to_vec(&body).unwrap(), "m").expect("the body decodes");

        assert!(matches!(answer.action, Action::Malformed { .. }));
    }

    #[test]
    fn a_body_that_is_not_json_is_an_error() {
        let refused = parse_answer(b"<html>not json</html>", "m").unwrap_err();

        assert!(matches!(refused, Error::Json(_)));
    }

    #[test]
    fn a_body_with_no_choices_is_an_error() {
        let refused = parse_answer(br#"{"usage":{"prompt_tokens":1}}"#, "m").unwrap_err();

        assert!(matches!(refused, Error::NoChoices));
    }

    #[test]
    fn a_message_content_becomes_the_prose() {
        let body = serde_json::json!({
            "choices": [{"message": {"content": "hello there"}, "finish_reason": "stop"}],
        });

        let answer = parse_answer(&serde_json::to_vec(&body).unwrap(), "m").unwrap();

        assert_eq!(answer.prose, "hello there");
        assert_eq!(answer.action, Action::Final);
    }

    #[test]
    fn a_message_without_content_has_no_prose_rather_than_an_error() {
        let body = serde_json::json!({
            "choices": [{"message": {}, "finish_reason": "stop"}],
        });

        let answer = parse_answer(&serde_json::to_vec(&body).unwrap(), "m").unwrap();

        assert_eq!(answer.prose, "");
    }

    #[test]
    fn prose_beyond_the_bound_is_clipped_on_a_character_boundary() {
        let long = "é".repeat(MAX_PROSE_BYTES);

        let clipped = clip_prose(&long);

        assert!(clipped.len() <= MAX_PROSE_BYTES);
        assert!(long.starts_with(&clipped));
    }

    #[test]
    fn a_turn_holding_a_line_renders_the_human_words_and_the_reply() {
        let turns = Context {
            system: "s".to_owned(),
            goal: "the goal".to_owned(),
            turns: vec![
                ContextTurn {
                    number: TurnNumber::new(1),
                    speech: Some("hello".to_owned()),
                    act: Some(ContextAct::Said {
                        reply: "hi".to_owned(),
                    }),
                },
                ContextTurn {
                    number: TurnNumber::new(2),
                    speech: Some("next".to_owned()),
                    act: None,
                },
            ],
        };

        let body = request_body(&turns, "m", 1);
        let content = body["messages"][1]["content"].as_str().unwrap();

        assert!(content.contains("human: hello"), "{content}");
        assert!(content.contains("reply: hi"), "{content}");
        assert!(content.contains("human: next"), "{content}");
    }

    #[test]
    fn a_length_finish_is_malformed_and_names_the_reason() {
        let body = serde_json::json!({
            "choices": [{"message": {"content": "cut off"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2},
        });

        let answer =
            parse_answer(&serde_json::to_vec(&body).unwrap(), "m").expect("the body decodes");

        let Action::Malformed { reason } = answer.action else {
            panic!("a length finish is malformed");
        };
        assert!(reason.contains("length"), "{reason}");
    }

    #[test]
    fn the_request_body_carries_the_model_the_tool_the_cap_and_both_messages() {
        let body = request_body(&context(), "kimi-k2.7-code", 4096);

        assert_eq!(body["model"], "kimi-k2.7-code");
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "run_shell");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["required"][0],
            "command",
        );
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["command"]["type"],
            "string"
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "the system prompt");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "the goal");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_first_call_carries_the_goal_as_a_user_message() {
        let body = request_body(&context(), "m", 1);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "the goal");
    }

    #[test]
    fn settled_turns_follow_the_goal_in_one_user_message() {
        let turns = Context {
            system: "s".to_owned(),
            goal: "the goal".to_owned(),
            turns: vec![ContextTurn {
                number: TurnNumber::new(1),
                speech: None,
                act: Some(ContextAct::Ran {
                    command: "ls".to_owned(),
                    observation: "exit code 0".to_owned(),
                }),
            }],
        };

        let body = request_body(&turns, "m", 1);

        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["role"], "user");
        let content = body["messages"][1]["content"].as_str().unwrap();
        assert!(content.contains("the goal"), "{content}");
        assert!(content.contains("ls"), "{content}");
        assert!(content.contains("exit code 0"), "{content}");
    }

    #[test]
    fn an_endpoint_without_a_port_defaults_to_https() {
        let endpoint = parse_endpoint("https://ollama.com/v1").unwrap();

        assert_eq!(endpoint.host, "ollama.com");
        assert_eq!(endpoint.port.get(), 443);
        assert_eq!(endpoint.request_path(), "/v1/chat/completions");
        assert_eq!(endpoint.rules(), Rules::public_only());
    }

    #[test]
    fn a_loopback_http_endpoint_parses_and_admits_loopback() {
        let endpoint = parse_endpoint("http://127.0.0.1:11434/v1").unwrap();

        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port.get(), 11434);
        assert_eq!(endpoint.request_path(), "/v1/chat/completions");
        assert_eq!(endpoint.scheme(), Scheme::Http);
        assert!(endpoint.rules().allows(AddressClass::Loopback));
        assert_eq!(endpoint.authority(), "127.0.0.1:11434");
    }

    #[test]
    fn localhost_is_a_loopback_cleartext_host() {
        let endpoint = parse_endpoint("http://localhost/v1").unwrap();

        assert_eq!(endpoint.host, "localhost");
        assert_eq!(endpoint.port.get(), 80);
        assert!(endpoint.rules().allows(AddressClass::Loopback));
    }

    #[test]
    fn an_ipv6_loopback_literal_parses_and_rebrackets_in_its_authority() {
        let endpoint = parse_endpoint("http://[::1]:11434/v1").unwrap();

        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.port.get(), 11434);
        assert!(endpoint.rules().allows(AddressClass::Loopback));
        assert_eq!(endpoint.authority(), "[::1]:11434");
    }

    #[test]
    fn a_cleartext_endpoint_that_is_not_loopback_is_refused() {
        let refused = parse_endpoint("http://ollama.com/v1").unwrap_err();

        assert!(matches!(refused, Error::EndpointCleartext { .. }));
        assert!(refused.to_string().contains("cleartext"), "{refused}");
        assert!(refused.to_string().contains("loopback"), "{refused}");
    }

    #[test]
    fn an_endpoint_that_is_neither_http_nor_https_is_refused() {
        assert!(matches!(
            parse_endpoint("ftp://ollama.com/v1"),
            Err(Error::EndpointScheme { .. }),
        ));
    }

    #[test]
    fn an_endpoint_with_a_port_and_a_trailing_slash_keeps_both() {
        let endpoint = parse_endpoint("https://ollama.com:11434/v1/").unwrap();

        assert_eq!(endpoint.port.get(), 11434);
        assert_eq!(endpoint.request_path(), "/v1/chat/completions");
    }

    #[test]
    fn an_endpoint_with_a_zero_port_is_refused() {
        assert!(matches!(
            parse_endpoint("https://ollama.com:0/v1"),
            Err(Error::EndpointForm { .. }),
        ));
    }

    #[test]
    fn the_credential_never_prints_its_value() {
        let directory = scratch("credential");
        let path = directory.join("credential");
        std::fs::write(&path, "s3cr3t\n").unwrap();

        let credential = Credential::read(&path).unwrap();

        assert_eq!(format!("{credential:?}"), "REDACTED");
        assert_eq!(credential.expose(), "s3cr3t");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_empty_credential_file_is_refused_by_name() {
        let directory = scratch("empty");
        let path = directory.join("credential");
        std::fs::write(&path, "   \n").unwrap();

        let refused = Credential::read(&path).unwrap_err();

        assert!(refused.to_string().contains(&path.display().to_string()));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Calls the real endpoint over TLS and checks it answers with the runtime's vocabulary.
    ///
    /// Ignored by default so the gate stays network-free. Run it by hand with a key in the
    /// environment:
    ///
    /// ```console
    /// OLLAMA_API_KEY=… cargo test -p mokmok-kernel --lib -- --ignored a_live_endpoint
    /// ```
    #[tokio::test]
    #[ignore = "reaches the network; run by hand with OLLAMA_API_KEY set"]
    async fn a_live_endpoint_answers_one_action() {
        let Ok(key) = std::env::var("OLLAMA_API_KEY") else {
            return;
        };
        let directory = scratch("live");
        let checkout = directory.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let credential = directory.join("credential");
        std::fs::write(&credential, key).unwrap();
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();

        let args = crate::config::Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            directory.join("data").to_str().unwrap(),
            "--confiner",
            "/bin/sh",
            "--allow-tool",
            "/bin/sh",
            "--allow-write",
            checkout.to_str().unwrap(),
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000009",
            "--goal",
            "list the source files",
            "--checkout",
            checkout.to_str().unwrap(),
            "--provider",
            "ollama",
            "--model",
            "kimi-k2.7-code",
            "--provider-credential-file",
            credential.to_str().unwrap(),
        ])
        .unwrap();
        let config = crate::config::Config::resolve(&args).unwrap();
        let session = mokmok_protocol::SessionID::from_uuid(uuid::Uuid::nil());
        let provider = super::Ollama::open(&config, session).unwrap();
        let prompt = Context {
            system: "You are a coding agent. List the current directory with the run_shell tool."
                .to_owned(),
            goal: "list the source files".to_owned(),
            turns: Vec::new(),
        };

        let answer = provider.complete(&prompt).await.unwrap();

        assert!(matches!(
            answer.action,
            Action::Command { .. } | Action::Final,
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Writes a `0600` credential file in `directory`, returning its path.
    fn credential_file(directory: &Path) -> PathBuf {
        let path = directory.join("credential");
        std::fs::write(&path, "s3cr3t").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        path
    }

    /// A provider configuration over `directory`, pointing at `endpoint`.
    fn provider_config(
        directory: &Path,
        endpoint: &str,
        credential: &Path,
    ) -> Config {
        let data = directory.join("data");
        let artifacts = data.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();

        Config {
            data_dir: data.clone(),
            events: data.join("events.sqlite"),
            artifacts,
            sandbox_root: data.join("sandbox"),
            allow_tool: Vec::new(),
            allow_path: Vec::new(),
            allow_write: Vec::new(),
            tool_env: Vec::new(),
            confiner: PathBuf::from("/bin/sh"),
            crash_after: None,
            provider: ProviderKind::Ollama,
            script: None,
            model: Some("m".to_owned()),
            endpoint: endpoint.to_owned(),
            provider_credential_file: Some(credential.to_path_buf()),
            max_answer_tokens: 16,
        }
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_off_the_validated_destination() {
        let directory = scratch("redirect");
        let credential = credential_file(&directory);

        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_port = target.local_addr().unwrap().port();
        let followed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&followed);
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = target.accept() else {
                return;
            };
            flag.store(true, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buffer = [0_u8; 1024];
            while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
            let body = br#"{"choices":[{"message":{"content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body),
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = origin.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buffer = [0_u8; 1024];
            while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{target_port}/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let config = provider_config(
            &directory,
            &format!("http://127.0.0.1:{origin_port}/v1"),
            &credential,
        );
        let provider = Ollama::open(&config, SessionID::from_uuid(Uuid::nil())).unwrap();

        let refused = provider.complete(&context()).await.unwrap_err();

        assert!(refused.to_string().contains("307"), "{refused}");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !followed.load(Ordering::SeqCst),
            "a redirect opened a second connection the destination was never validated for",
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}

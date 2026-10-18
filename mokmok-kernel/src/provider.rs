//! The real provider: an OpenAI-compatible chat-completions call over TLS.
//!
//! The provider is trusted configuration rather than a capability, so its destination is validated
//! through [`mokmok_egress`] before any socket exists and the validated addresses are pinned into
//! the HTTP client. Nothing of the wire crosses the runtime's [`Provider`] port: the request is
//! built here, the answer is parsed back into the runtime's [`Answer`], and the credential
//! resolution answered for `env_key` arrives as the [`Config`]'s own field, in a value whose
//! [`Debug`] prints `REDACTED`.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use mokmok_agent_runtime::{
    Action, Answer, ArtifactRef, Context, ContextAct, ContextTurn, Error as AgentError, Provider,
    Usage, clip_prose, clip_query,
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
use crate::config::{Config, Credential};

const SHELL_TOOL: &str = "run_shell";

const FIND_TOOL: &str = "find_files";

const EDIT_TOOL: &str = "apply_patch";

const USE_SKILL_TOOL: &str = "use_skill";

const SHELL_DESCRIPTION: &str = "Run one shell command with `sh -c` in a fresh, empty working \
     directory. The checkout is reached by absolute path, stdin is empty, and there is no TCP \
     network.";

const FIND_DESCRIPTION: &str = "Find files under the granted search roots by the words in their \
     name or their contents. Returns a ranked list of paths. Reads and changes nothing.";

const EDIT_DESCRIPTION: &str = "Apply a unified diff to one file under the checkout, replacing \
     that file's bytes. The diff's --- and +++ labels name the file relative to the checkout; \
     the tool never follows a symlink. One file per call, and the patch applies cleanly or \
     nothing is written. An optional expect carries the sha256 the model believes the file \
     holds; the edit is refused if the file does not match.";

const USE_SKILL_DESCRIPTION: &str = "Load one advertised skill's instructions by name. The name \
     must be one the system prompt lists under \"Skills you may load by name with use_skill\"; the \
     instructions then ride every later prompt for the session.";

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
    /// The stream reported an error event.
    #[error("the endpoint reported a streaming error: {message}")]
    StreamEvent { message: String },
    /// The stream ended without any content or tool call.
    #[error("the stream ended without an answer")]
    EmptyStream,
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
    /// Builds the provider: resolves and validates the endpoint before any socket exists, and pins
    /// the validated addresses into the HTTP client.
    ///
    /// `credential` is the value resolution answered for `env_key`, handed in rather than read here:
    /// the provider has no configuration file, so it cannot name the key a refusal would have to,
    /// and the environment is read at the binary's boundary and nowhere else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoModel`] when no model is named, the errors of [`parse_endpoint`],
    /// [`Error::Resolve`] when the host cannot be resolved, [`Error::Destination`] when validation
    /// refuses the destination, [`Error::Audit`] when the audit journal cannot be opened,
    /// [`Error::Artifact`] when the answer store cannot be opened, and [`Error::Client`] when the
    /// HTTP client cannot be built.
    pub fn open(
        config: &Config,
        credential: Credential,
        session: SessionID,
    ) -> Result<Self, Error> {
        let model = config
            .provider
            .model
            .clone()
            .filter(|model| !model.is_empty())
            .ok_or(Error::NoModel)?;
        let endpoint = parse_endpoint(&config.provider.endpoint)?;
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
            max_tokens: config.provider.max_answer_tokens,
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

    async fn complete_watched<'a>(
        &'a self,
        context: &'a Context,
        watch: &'a mut dyn FnMut(&str),
    ) -> Result<Answer, AgentError> {
        let mut body_value = request_body(context, &self.model, self.max_tokens);
        if let Some(obj) = body_value.as_object_mut() {
            obj.insert("stream".to_owned(), serde_json::Value::Bool(true));
        }
        let body = serde_json::to_vec(&body_value)
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

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.contains("text/event-stream") {
            let bytes = response
                .bytes()
                .await
                .map_err(|error| provider_error(Error::Transport(error)))?;
            let answer = parse_answer(&bytes, &self.model).map_err(provider_error)?;
            self.store
                .put(&bytes)
                .map_err(|error| provider_error(Error::Artifact(error)))?;
            watch(&answer.prose);
            return Ok(answer);
        }

        let (answer, bytes) = stream_answer(response, watch, &self.model).await?;
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

async fn stream_answer(
    response: reqwest::Response,
    watch: &mut dyn FnMut(&str),
    model: &str,
) -> Result<(Answer, Vec<u8>), AgentError> {
    let mut content = String::new();
    let mut tool_calls: BTreeMap<u32, serde_json::Value> = BTreeMap::new();
    let mut finish_reason = String::new();
    let mut usage: Option<serde_json::Value> = None;
    let mut buffer = Vec::new();
    let mut event = Vec::new();

    let mut stream = response.bytes_stream();
    'stream: while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| provider_error(Error::Transport(error)))?;
        buffer.extend_from_slice(&chunk);
        while let Some(pos) = buffer.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..pos).collect();
            let line = trim_carriage_return(&line);

            if line.is_empty() {
                if dispatch(
                    &event,
                    &mut content,
                    &mut tool_calls,
                    &mut finish_reason,
                    &mut usage,
                    watch,
                )? == Dispatch::Done
                {
                    break 'stream;
                }
                event.clear();
            } else if let Some(data) = data_line(line) {
                if !event.is_empty() {
                    event.push(b'\n');
                }
                event.extend_from_slice(data);
            }

            let _ = buffer.drain(..1_usize);
        }
    }

    if !buffer.is_empty()
        && let Some(data) = data_line(trim_carriage_return(&buffer))
    {
        if !event.is_empty() {
            event.push(b'\n');
        }
        event.extend_from_slice(data);
    }
    let _ = dispatch(
        &event,
        &mut content,
        &mut tool_calls,
        &mut finish_reason,
        &mut usage,
        watch,
    )?;

    if content.is_empty() && tool_calls.is_empty() {
        return Err(provider_error(Error::EmptyStream));
    }

    build_answer(&content, tool_calls, &finish_reason, usage, model)
}

/// Whether a dispatched event ended the stream or left more of it to read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dispatch {
    More,
    Done,
}

fn dispatch(
    event: &[u8],
    content: &mut String,
    tool_calls: &mut BTreeMap<u32, serde_json::Value>,
    finish_reason: &mut String,
    usage: &mut Option<serde_json::Value>,
    watch: &mut dyn FnMut(&str),
) -> Result<Dispatch, AgentError> {
    if event.is_empty() {
        return Ok(Dispatch::More);
    }
    if event == b"[DONE]" {
        return Ok(Dispatch::Done);
    }
    apply_event(event, content, tool_calls, finish_reason, usage, watch)?;

    Ok(Dispatch::More)
}

fn data_line(line: &[u8]) -> Option<&[u8]> {
    line.strip_prefix(b"data:").map(trim_leading_space)
}

fn trim_carriage_return(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn trim_leading_space(event: &[u8]) -> &[u8] {
    event.strip_prefix(b" ").unwrap_or(event)
}

fn apply_event(
    event: &[u8],
    content: &mut String,
    tool_calls: &mut BTreeMap<u32, serde_json::Value>,
    finish_reason: &mut String,
    usage: &mut Option<serde_json::Value>,
    watch: &mut dyn FnMut(&str),
) -> Result<(), AgentError> {
    let value: serde_json::Value =
        serde_json::from_slice(event).map_err(|error| provider_error(Error::Json(error)))?;
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        return Err(provider_error(Error::StreamEvent {
            message: message.to_owned(),
        }));
    }

    if let Some(delta) = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("delta"))
    {
        if let Some(fragment) = delta
            .get("content")
            .and_then(serde_json::Value::as_str)
            .filter(|fragment| !fragment.is_empty())
        {
            watch(fragment);
            content.push_str(fragment);
        }
        if let Some(deltas) = delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for delta_call in deltas {
                if let Some(index) = delta_call.get("index").and_then(serde_json::Value::as_u64) {
                    let index = u32::try_from(index).unwrap_or(u32::MAX);
                    let entry = tool_calls
                        .entry(index)
                        .or_insert_with(|| serde_json::json!({}));
                    merge_tool_call(entry, delta_call);
                }
            }
        }
    }

    if let Some(finish) = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(serde_json::Value::as_str)
    {
        finish.clone_into(finish_reason);
    }
    if let Some(usage_value) = value.get("usage").filter(|value| !value.is_null()) {
        *usage = Some(usage_value.clone());
    }

    Ok(())
}

fn build_answer(
    content: &str,
    tool_calls: BTreeMap<u32, serde_json::Value>,
    finish_reason: &str,
    usage: Option<serde_json::Value>,
    model: &str,
) -> Result<(Answer, Vec<u8>), AgentError> {
    // A stream may close without a finish event. The content it sent is complete, so name the
    // finish the content itself implies rather than reporting every such answer as malformed.
    let finish = if finish_reason.is_empty() {
        if tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
    } else {
        finish_reason
    };
    let mut message = serde_json::json!({
        "role": "assistant",
        "content": content,
    });
    if !tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = tool_calls.into_values().collect();
        if let Some(object) = message.as_object_mut() {
            let _ = object.insert("tool_calls".to_owned(), serde_json::Value::Array(calls));
        }
    }
    let mut body = serde_json::json!({
        "choices": [{"message": message, "finish_reason": finish}],
    });
    if let Some(usage_value) = usage
        && let Some(object) = body.as_object_mut()
    {
        let _ = object.insert("usage".to_owned(), usage_value);
    }
    let bytes = serde_json::to_vec(&body).map_err(|error| provider_error(Error::Encode(error)))?;
    let answer = parse_answer(&bytes, model).map_err(provider_error)?;

    Ok((answer, bytes))
}

fn merge_tool_call(
    base: &mut serde_json::Value,
    delta: &serde_json::Value,
) {
    let Some(delta_object) = delta.as_object() else {
        return;
    };
    let Some(base_object) = base.as_object_mut() else {
        return;
    };

    for (key, value) in delta_object {
        match key.as_str() {
            "function" => {
                let entry = base_object
                    .entry("function")
                    .or_insert_with(|| serde_json::json!({}));
                merge_function(entry, value);
            },
            _ => {
                let _ = base_object.insert(key.clone(), value.clone());
            },
        }
    }
}

fn merge_function(
    base: &mut serde_json::Value,
    delta: &serde_json::Value,
) {
    let Some(delta_object) = delta.as_object() else {
        return;
    };
    let Some(base_object) = base.as_object_mut() else {
        return;
    };

    for (key, value) in delta_object {
        if key == "arguments" {
            let mut joined = base_object
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .map_or_else(String::new, String::from);
            if let Some(appended) = value.as_str() {
                joined.push_str(appended);
            }
            let _ = base_object.insert(key.clone(), serde_json::Value::String(joined));
        } else {
            let _ = base_object.insert(key.clone(), value.clone());
        }
    }
}

/// The chat-completions body the endpoint was measured to accept.
///
/// The tool list is the runtime's, and each call's `arguments` are the JSON-string envelope that
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
            {"role": "system", "content": system_text(context)},
            {"role": "user", "content": user_text(context)},
        ],
        "tools": tools(context),
        "max_tokens": max_tokens,
        "stream": false,
    })
}

/// The loaded bodies ride the system message rather than the user text, so progressive disclosure
/// adds them once, after the prompt the session was opened with, and a later turn's user message
/// stays the goal and the turns it always was.
fn system_text(context: &Context) -> String {
    let mut text = context.system.clone();
    for skill in &context.loaded {
        text.push_str("\n\nSkill `");
        text.push_str(skill.name.as_str());
        text.push_str("`:\n");
        text.push_str(&skill.instructions);
    }

    text
}

/// The tools the request offers, in the order the model reads them.
///
/// The finder is offered exactly when [`Context::search`] says the session holds a granted root,
/// which is the same recorded flag the system prompt names it by: a body offering a tool the
/// finder will not answer for would leave the model asking into a refusal.
fn tools(context: &Context) -> Vec<serde_json::Value> {
    let mut tools = vec![shell_tool()];
    if context.search {
        tools.push(find_tool());
    }
    if context.writable {
        tools.push(edit_tool());
    }
    if context.skills {
        tools.push(use_skill_tool());
    }

    tools
}

fn shell_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": SHELL_TOOL,
            "description": SHELL_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            },
        },
    })
}

fn find_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": FIND_TOOL,
            "description": FIND_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
            },
        },
    })
}

fn edit_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": EDIT_TOOL,
            "description": EDIT_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "A unified diff: a --- label, a +++ label, then @@ hunks.",
                    },
                    "expect": {
                        "type": "string",
                        "description": "Optional. The sha256 of the bytes you believe the file holds. The edit is refused if the file does not match.",
                    },
                },
                "required": ["patch"],
            },
        },
    })
}

fn use_skill_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": USE_SKILL_TOOL,
            "description": USE_SKILL_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
        },
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
        // A find is labelled as itself: `command` would tell the model a search ran a program.
        Some(ContextAct::Found { query, observation }) => {
            text.push_str("\nfind: ");
            text.push_str(query);
            text.push_str("\nobservation: ");
            text.push_str(observation);
        },
        // An edit names the file in its own observation, so no path rides beside it.
        Some(ContextAct::Edited { observation }) => {
            text.push_str("\nedit: ");
            text.push_str(observation);
        },
        // A load names the skill whose body now rides the system message; a refusal names the reason
        // it did not, so the model can tell the two without re-reading the instructions block.
        Some(ContextAct::Loaded { name }) => {
            text.push_str("\nskill loaded: ");
            text.push_str(name.as_str());
        },
        Some(ContextAct::Refused { name, reason }) => {
            text.push_str("\nskill refused: ");
            text.push_str(name.as_str());
            text.push_str("\nreason: ");
            text.push_str(reason);
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

/// The action the first structured tool call names, or why it could not be read.
///
/// The function name is what selects the decoder, so a call naming a tool this session was offered
/// no way to reach is malformed rather than silently read as a command.
fn tool_call(message: &serde_json::Value) -> Action {
    let Some(function) = message
        .get("tool_calls")
        .and_then(|calls| calls.get(0))
        .and_then(|call| call.get("function"))
    else {
        return Action::Malformed {
            reason: "the answer named no tool call".to_owned(),
        };
    };
    let Some(name) = function.get("name").and_then(serde_json::Value::as_str) else {
        return Action::Malformed {
            reason: "the tool call names no function".to_owned(),
        };
    };
    let Some(arguments) = function
        .get("arguments")
        .and_then(serde_json::Value::as_str)
    else {
        return Action::Malformed {
            reason: format!("the {name} call named no tool call arguments"),
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

    match name {
        SHELL_TOOL => decoded
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || Action::Malformed {
                    reason: "the tool arguments name no command string".to_owned(),
                },
                |command| Action::Command {
                    command: command.to_owned(),
                },
            ),
        FIND_TOOL => decoded
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || Action::Malformed {
                    reason: "the tool arguments name no query string".to_owned(),
                },
                // The clip happens here, so the answer, the record, and the search all name one
                // string rather than three lengths of it.
                |query| Action::Find {
                    query: clip_query(query),
                },
            ),
        EDIT_TOOL => mokmok_agent_runtime::patch_refusal(arguments).map_or_else(
            || {
                decoded
                    .get("patch")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || Action::Malformed {
                            reason: "the tool arguments name no patch string".to_owned(),
                        },
                        |patch| Action::Edit {
                            patch: patch.to_owned(),
                            expect: decoded
                                .get("expect")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                        },
                    )
            },
            |reason| Action::Malformed { reason },
        ),
        USE_SKILL_TOOL => decoded
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || Action::Malformed {
                    reason: "the tool arguments name no skill name string".to_owned(),
                },
                |name| match mokmok_agent_runtime::SkillName::new(name) {
                    Ok(name) => Action::UseSkill { name },
                    Err(refusal) => Action::Malformed {
                        reason: format!("the skill name is not valid: {refusal}"),
                    },
                },
            ),
        other => Action::Malformed {
            reason: format!("the answer called {other:?}, which is not a tool of this session"),
        },
    }
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
        Action, Credential, Error, Ollama, Permit, PolicyClient, Usage, parse_answer,
        parse_endpoint, request_body,
    };
    use crate::config::{Config, ProviderKind, ProviderSettings};
    use clap::Parser as _;
    use mokmok_agent_runtime::{
        Context, ContextAct, ContextSkill, ContextTurn, MAX_PROSE_BYTES, MAX_QUERY_BYTES,
        Provider as _, SkillName, clip_prose,
    };
    use mokmok_egress::{
        AddressClass, CallContext, Destination, Host, Port, Refusal, Rules, Scheme,
    };
    use mokmok_observability::{AuditKind, AuditLog as _, Journal, Outcome};
    use mokmok_protocol::{ExecutionID, Moment, SessionID};
    use mokmok_state_machine::TurnNumber;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use uuid::Uuid;

    /// The measured tool-call response, kept as the fixture it is rather than re-encoded.
    const TOOL_CALLS: &str = include_str!("../testdata/probe-tool-calls.json");

    /// The measured tool-result response, the same way.
    const TOOL_RESULT: &str = include_str!("../testdata/probe-tool-result.json");

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
            search: false,
            writable: false,
            skills: false,
            loaded: Vec::new(),
        }
    }

    fn searching() -> Context {
        Context {
            search: true,
            writable: false,
            ..context()
        }
    }

    fn writable() -> Context {
        Context {
            writable: true,
            ..context()
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

    fn call_body(
        name: &str,
        arguments: &str,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{"function": {"name": name, "arguments": arguments}}],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2},
        }))
        .unwrap()
    }

    #[test]
    fn a_find_call_decodes_to_its_query_and_clips_it() {
        let answer = parse_answer(&call_body("find_files", r#"{"query": "bm25 index"}"#), "m")
            .expect("the body decodes");

        assert_eq!(
            answer.action,
            Action::Find {
                query: "bm25 index".to_owned(),
            },
        );

        let long = "q".repeat(MAX_QUERY_BYTES + 8);
        let clipped = parse_answer(
            &call_body("find_files", &format!(r#"{{"query": "{long}"}}"#)),
            "m",
        )
        .expect("the body decodes");

        assert_eq!(
            clipped.action,
            Action::Find {
                query: "q".repeat(MAX_QUERY_BYTES),
            },
        );
    }

    #[test]
    fn a_find_call_without_a_query_string_is_malformed() {
        let answer =
            parse_answer(&call_body("find_files", r#"{"path": "notes.txt"}"#), "m").unwrap();

        assert!(matches!(answer.action, Action::Malformed { .. }));
    }

    #[test]
    fn a_call_naming_a_tool_this_session_offers_not_is_malformed_and_names_it() {
        let answer =
            parse_answer(&call_body("rm_rf", r#"{"query": "x"}"#), "m").expect("the body decodes");

        let Action::Malformed { reason } = answer.action else {
            panic!("a call to another tool is malformed");
        };
        assert!(reason.contains("rm_rf"), "{reason}");
    }

    #[test]
    fn tool_arguments_that_are_not_json_are_malformed_rather_than_retried() {
        let answer =
            parse_answer(&call_body("run_shell", "{not json"), "m").expect("the body decodes");

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
            search: false,
            writable: false,
            skills: false,
            loaded: Vec::new(),
        };

        let body = request_body(&turns, "m", 1);
        let content = body["messages"][1]["content"].as_str().unwrap();

        assert!(content.contains("human: hello"), "{content}");
        assert!(content.contains("reply: hi"), "{content}");
        assert!(content.contains("human: next"), "{content}");
    }

    #[test]
    fn a_found_turn_renders_its_query_and_its_observation_under_their_own_labels() {
        let turns = Context {
            system: "s".to_owned(),
            goal: "the goal".to_owned(),
            turns: vec![ContextTurn {
                number: TurnNumber::new(1),
                speech: None,
                act: Some(ContextAct::Found {
                    query: "quokka".to_owned(),
                    observation: "indexed=2 hits=1".to_owned(),
                }),
            }],
            search: true,
            writable: false,
            skills: false,
            loaded: Vec::new(),
        };

        let body = request_body(&turns, "m", 1);
        let content = body["messages"][1]["content"].as_str().unwrap();

        assert!(content.contains("find: quokka"), "{content}");
        assert!(
            content.contains("observation: indexed=2 hits=1"),
            "{content}"
        );
        assert!(
            !content.contains("command:"),
            "a find must not read as a command: {content}",
        );
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
        assert_eq!(
            body["tools"].as_array().unwrap().len(),
            1,
            "a session with no granted root is offered no finder",
        );
    }

    #[test]
    fn the_body_offers_the_finder_exactly_when_the_session_holds_a_root() {
        let one_tool = request_body(&context(), "m", 1);
        assert_eq!(one_tool["tools"].as_array().unwrap().len(), 1);

        let two = request_body(&searching(), "m", 1);
        let tools = two["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "run_shell");
        assert_eq!(tools[1]["type"], "function");
        assert_eq!(tools[1]["function"]["name"], "find_files");
        assert_eq!(
            tools[1]["function"]["parameters"]["properties"]["query"]["type"],
            "string",
        );
        assert_eq!(tools[1]["function"]["parameters"]["required"][0], "query",);
        assert!(
            tools[1]["function"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("contents")),
            "the finder's description must say it reads a file's contents",
        );
    }

    #[test]
    fn the_body_offers_the_editor_exactly_when_the_session_holds_a_writable_checkout() {
        let editable = request_body(&writable(), "m", 1);
        let tools = editable["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "run_shell");
        assert_eq!(tools[1]["function"]["name"], "apply_patch");
        assert_eq!(tools[1]["function"]["parameters"]["required"][0], "patch",);
        assert!(
            tools[1]["function"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("unified diff")),
            "the editor's description must say it takes a unified diff",
        );
    }

    fn skilled() -> Context {
        Context {
            skills: true,
            ..context()
        }
    }

    #[test]
    fn the_body_offers_use_skill_exactly_when_the_session_advertises_a_skill() {
        let bare = request_body(&context(), "m", 1);
        assert_eq!(bare["tools"].as_array().unwrap().len(), 1);

        let offered = request_body(&skilled(), "m", 1);
        let tools = offered["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1]["function"]["name"], "use_skill");
        assert_eq!(
            tools[1]["function"]["parameters"]["properties"]["name"]["type"],
            "string",
        );
        assert_eq!(tools[1]["function"]["parameters"]["required"][0], "name");
    }

    #[test]
    fn a_use_skill_call_decodes_to_the_named_action() {
        let answer = parse_answer(
            &call_body("use_skill", r#"{"name": "pdf-processing"}"#),
            "m",
        )
        .unwrap();

        assert_eq!(
            answer.action,
            Action::UseSkill {
                name: SkillName::new("pdf-processing").unwrap(),
            },
        );
    }

    #[test]
    fn a_use_skill_call_naming_an_invalid_skill_is_malformed() {
        let answer = parse_answer(&call_body("use_skill", r#"{"name": "Bad Name"}"#), "m").unwrap();

        assert!(matches!(answer.action, Action::Malformed { .. }));
    }

    #[test]
    fn a_use_skill_call_naming_no_skill_is_malformed() {
        let answer = parse_answer(&call_body("use_skill", "{}"), "m").unwrap();

        assert!(matches!(answer.action, Action::Malformed { .. }));
    }

    #[test]
    fn the_system_message_carries_every_loaded_skill_after_the_base_prompt() {
        let loaded = Context {
            skills: true,
            loaded: vec![ContextSkill {
                name: SkillName::new("pdf-processing").unwrap(),
                instructions: "do the thing".to_owned(),
            }],
            ..context()
        };

        let body = request_body(&loaded, "m", 1);
        let system = body["messages"][0]["content"].as_str().unwrap();

        assert!(system.starts_with("the system prompt"), "{system}");
        assert!(
            system.contains("Skill `pdf-processing`:\ndo the thing"),
            "the loaded body rides the system message after the base prompt: {system}",
        );
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
            search: false,
            writable: false,
            skills: false,
            loaded: Vec::new(),
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
        if std::env::var_os("OLLAMA_API_KEY").is_none() {
            return;
        }
        let directory = scratch("live");
        let checkout = directory.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();

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
            "--env-key",
            "OLLAMA_API_KEY",
        ])
        .unwrap();
        // The key is the test process's own, and resolution is handed the environment the process
        // was started with rather than a copy of one variable.
        let config =
            crate::config::Config::resolve(&args, None, &crate::config::ProcessEnvironment::read())
                .unwrap();
        let session = mokmok_protocol::SessionID::from_uuid(uuid::Uuid::nil());
        let credential = config
            .provider
            .credential
            .clone()
            .expect("the run named the key it holds");
        let provider = super::Ollama::open(&config, credential, session).unwrap();
        let prompt = Context {
            system: "You are a coding agent. List the current directory with the run_shell tool."
                .to_owned(),
            goal: "list the source files".to_owned(),
            turns: Vec::new(),
            search: false,
            writable: false,
            skills: false,
            loaded: Vec::new(),
        };

        let answer = provider.complete(&prompt).await.unwrap();

        assert!(matches!(
            answer.action,
            Action::Command { .. } | Action::Final,
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A provider configuration over `directory`, pointing at `endpoint`, with a hardcoded
    /// credential.
    fn provider_config(
        directory: &Path,
        endpoint: &str,
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
            search_root: Vec::new(),
            search_index: data.join("search"),
            skill_root: Vec::new(),
            tool_env: Vec::new(),
            confiner: PathBuf::from("/bin/sh"),
            crash_after: None,
            provider: ProviderSettings {
                kind: ProviderKind::Ollama,
                model: Some("m".to_owned()),
                endpoint: endpoint.to_owned(),
                credential: Some(Credential::new("s3cr3t".to_owned())),
                max_answer_tokens: 16,
                ..ProviderSettings::default()
            },
        }
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_off_the_validated_destination() {
        let directory = scratch("redirect");

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

        let config = provider_config(&directory, &format!("http://127.0.0.1:{origin_port}/v1"));
        let credential = config.provider.credential.clone().unwrap();
        let provider =
            Ollama::open(&config, credential, SessionID::from_uuid(Uuid::nil())).unwrap();

        let refused = provider.complete(&context()).await.unwrap_err();

        assert!(refused.to_string().contains("307"), "{refused}");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !followed.load(Ordering::SeqCst),
            "a redirect opened a second connection the destination was never validated for",
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// One destination over `host` and `port` on the cleartext transport.
    fn destination(
        host: Host,
        port: Port,
    ) -> Destination {
        Destination {
            scheme: Scheme::Http,
            host,
            port,
        }
    }

    #[test]
    fn the_policy_refuses_a_destination_that_only_shares_the_port() {
        let address = "127.0.0.1".parse().expect("a loopback address");
        let port = Port::new(8080).expect("a port");
        let permit = Permit {
            host: Host::Ip(address),
            port,
        };
        let context = CallContext {
            principal: crate::authority::AGENT,
            execution: ExecutionID::from_uuid(Uuid::nil()),
        };

        assert_eq!(
            permit.check(
                &destination(Host::Name("elsewhere.test".to_owned()), port),
                &context
            ),
            Err(Refusal::Denied),
            "a name that is not the pinned host is refused however well the port matches",
        );
        assert_eq!(
            permit.check(&destination(Host::Ip(address), port), &context),
            Ok(()),
            "and the pinned host and port together are what it admits",
        );
    }

    #[test]
    fn opening_the_provider_audits_the_destination_it_validated() {
        let directory = scratch("provider-audit");
        // A bound port is a free one. Nothing connects to it: opening the provider only resolves and
        // validates the destination.
        let bound = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = bound.local_addr().unwrap().port();
        let endpoint = format!("http://127.0.0.1:{port}/v1");
        let config = provider_config(&directory, &endpoint);
        let credential = config.provider.credential.clone().unwrap();

        let _provider =
            Ollama::open(&config, credential, SessionID::from_uuid(Uuid::nil())).unwrap();

        let journal = Journal::open(config.data_dir.join("audit.jsonl")).unwrap();
        let entries = journal.read(Moment::from_millis(0)).unwrap();

        assert_eq!(
            entries.len(),
            1,
            "the decision that pinned the destination is the one entry: {entries:#?}",
        );
        assert_eq!(entries[0].kind(), AuditKind::Egress);
        assert_eq!(entries[0].outcome(), Outcome::Allowed);
        let detail = format!("model provider 127.0.0.1:{port}");
        assert_eq!(entries[0].detail(), Some(detail.as_str()));

        let _ = std::fs::remove_dir_all(&directory);
    }

    fn serve(
        response_body: &[u8],
        content_type: &str,
        assert_stream: bool,
    ) -> (Ollama, PathBuf) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        let directory = scratch(&format!("stream-{count}"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = response_body.to_vec();
        let content_type = content_type.to_owned();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buffer[..n]);
                    },
                }
            }
            if assert_stream {
                let headers_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map_or(request.len(), |position| position + 4);
                let request_body = &request[headers_end..];
                let parsed: serde_json::Value = serde_json::from_slice(request_body).unwrap();
                assert_eq!(parsed["stream"], serde_json::Value::Bool(true));
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                body.len(),
                String::from_utf8_lossy(&body),
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let endpoint = format!("http://127.0.0.1:{port}/v1");
        let config = provider_config(&directory, &endpoint);
        let credential = config.provider.credential.clone().unwrap();
        let provider =
            Ollama::open(&config, credential, SessionID::from_uuid(Uuid::nil())).unwrap();

        (provider, directory)
    }

    fn sse(events: impl IntoIterator<Item = serde_json::Value>) -> Vec<u8> {
        let mut body = Vec::new();
        for event in events {
            body.extend_from_slice(b"data: ");
            body.extend_from_slice(serde_json::to_vec(&event).unwrap().as_slice());
            body.extend_from_slice(b"\n\n");
        }
        body.extend_from_slice(b"data: [DONE]\n\n");

        body
    }

    #[tokio::test]
    async fn streamed_fragments_arrive_in_order_and_match_the_whole_body() {
        let body = sse([
            serde_json::json!({"choices": [{"delta": {"content": "hello "}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {"content": "world"}, "finish_reason": null}]}),
            serde_json::json!({
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2},
            }),
        ]);
        let (provider, directory) = serve(&body, "text/event-stream", true);
        let mut seen = Vec::new();

        let answer = provider
            .complete_watched(&context(), &mut |fragment| seen.push(fragment.to_owned()))
            .await
            .unwrap();

        assert_eq!(seen, vec!["hello ".to_owned(), "world".to_owned()]);
        assert_eq!(answer.prose, "hello world");
        assert_eq!(answer.action, Action::Final);
        assert_eq!(
            answer.usage,
            Usage {
                input: 3,
                output: 2,
            },
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_tool_call_split_across_stream_fragments_decodes_to_command() {
        let body = sse([
            serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "run_shell", "arguments": ""}}]}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"command\":\""}}]}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "ls\"}"}}]}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ]);
        let (provider, directory) = serve(&body, "text/event-stream", true);

        let answer = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(
            answer.action,
            Action::Command {
                command: "ls".to_owned(),
            },
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_non_streaming_json_response_still_parses_and_watches_once() {
        let body = br#"{"choices":[{"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        let (provider, directory) = serve(body, "application/json", true);
        let mut seen = Vec::new();

        let answer = provider
            .complete_watched(&context(), &mut |fragment| seen.push(fragment.to_owned()))
            .await
            .unwrap();

        assert_eq!(seen, vec!["done".to_owned()]);
        assert_eq!(answer.prose, "done");
        assert_eq!(answer.action, Action::Final);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_stream_error_event_is_a_provider_error() {
        let body = sse([serde_json::json!({"error": {"message": "model overloaded"}})]);
        let (provider, directory) = serve(&body, "text/event-stream", true);

        let refused = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap_err();

        assert!(
            refused.to_string().contains("model overloaded"),
            "{refused}",
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_stream_with_no_content_or_tool_call_is_a_provider_error() {
        let body = sse([serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})]);
        let (provider, directory) = serve(&body, "text/event-stream", true);

        let refused = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap_err();

        assert!(
            refused
                .to_string()
                .contains("stream ended without an answer"),
            "{refused}",
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_streaming_request_body_sets_stream_true_while_request_body_keeps_false() {
        let non_streaming = request_body(&context(), "m", 1);
        assert_eq!(non_streaming["stream"], serde_json::Value::Bool(false));
    }

    #[tokio::test]
    async fn a_done_event_ends_the_call_before_the_connection_does() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buffer = [0_u8; 4096];
            while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
            );
            let _ = stream.write_all(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n",
            );
            let _ = stream.write_all(b"data: [DONE]\n\n");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_secs(3));
        });
        let directory = scratch("done-holds");
        let config = provider_config(&directory, &format!("http://127.0.0.1:{port}/v1"));
        let credential = config.provider.credential.clone().unwrap();
        let provider =
            Ollama::open(&config, credential, SessionID::from_uuid(Uuid::nil())).unwrap();

        let answer = tokio::time::timeout(
            Duration::from_secs(1),
            provider.complete_watched(&context(), &mut |_| {}),
        )
        .await
        .expect("the call ends on [DONE] rather than on the connection")
        .unwrap();

        assert_eq!(answer.prose, "done");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_stream_without_a_finish_reason_is_a_final_answer() {
        let body = sse([serde_json::json!({"choices": [{"delta": {"content": "done"}}]})]);
        let (provider, directory) = serve(&body, "text/event-stream", true);

        let answer = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(answer.action, Action::Final);
        assert_eq!(answer.prose, "done");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn an_unterminated_final_event_is_still_read() {
        let body =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"tail\"},\"finish_reason\":\"stop\"}]}";
        let (provider, directory) = serve(body, "text/event-stream", true);

        let answer = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(answer.prose, "tail");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn an_unterminated_done_event_is_not_an_error() {
        let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"tail\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]";
        let (provider, directory) = serve(body, "text/event-stream", true);

        let answer = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(answer.prose, "tail");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_multi_line_event_is_joined_before_it_is_decoded() {
        let body = b": keep-alive\ndata: {\"choices\":[{\"delta\":{\"content\":\"multi\"},\ndata: \"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
        let (provider, directory) = serve(body, "text/event-stream", true);

        let answer = provider
            .complete_watched(&context(), &mut |_| {})
            .await
            .unwrap();

        assert_eq!(answer.prose, "multi");
        let _ = std::fs::remove_dir_all(&directory);
    }
}

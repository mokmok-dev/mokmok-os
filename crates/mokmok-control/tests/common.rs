//! A scripted ACP agent shared by the control plane's integration tests.
//!
//! The agent speaks just enough of the protocol to prove a delegation: it initializes, creates a
//! session, streams one message chunk, asks for permission, and ends the turn. Modes let a test
//! make it refuse the prompt, go silent, ask for another tool kind, or record what it saw.

#![allow(dead_code)]

use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mokmok_acp::{
    Handler, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification,
};
use serde_json::{Value, json};

/// The session this agent reports from `session/new`.
pub const SESSION_ID: &str = "fake-session";

/// The exact text `--stream` spells one character at a time.
pub const STREAM_SENTENCE: &str = "The agent streamed its answer in tiny chunks so the runner could prove that many pieces collapse into one transcript entry, that the summary keeps the whole answer, and that the dashboard displays it.";

/// The exact text `--stream-thoughts` spells one character at a time.
pub const STREAM_THOUGHT: &str = "The thought buffer coalesces the same way, keeping its own order after the message and before any tool call.";

/// What an integration test returns.
pub type TestError = Box<dyn Error + Send + Sync>;
pub type TestResult = Result<(), TestError>;

/// Runs one test body on its own thread and fails it after ten seconds.
///
/// A client that stops pumping messages would otherwise block until cargo-mutants' own timeout.
pub fn within_deadline<F>(run: F) -> TestResult
where
    F: FnOnce() -> TestResult + Send + 'static,
{
    let (finished, deadline) = mpsc::channel();
    let handle = thread::spawn(move || {
        let _ = finished.send(run());
    });
    let result = deadline
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| {
            Err("the test did not finish within ten seconds"
                .to_owned()
                .into())
        });
    drop(handle);
    result
}

/// A handler that counts what the agent sent and refuses every permission request.
#[derive(Default)]
pub struct Recorder {
    pub updates: usize,
    pub permissions: usize,
}

impl Handler for Recorder {
    fn session_update(
        &mut self,
        _update: SessionNotification,
    ) {
        self.updates += 1;
    }

    fn request_permission(
        &mut self,
        _request: RequestPermissionRequest,
    ) -> RequestPermissionResponse {
        self.permissions += 1;
        RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
    }
}

/// One bidirectional message channel used by the scripted agent.
pub trait Link {
    /// Sends one JSON-RPC message.
    fn send(
        &mut self,
        message: &str,
    );

    /// Receives one JSON-RPC message, or `None` when the peer closed the channel.
    fn recv(&mut self) -> Option<String>;
}

/// How the scripted agent behaves.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // One switch per test mode, never combined state.
pub struct Script {
    /// Answer `session/prompt` with an error.
    pub fail: bool,
    /// Answer the handshake and then never answer the prompt.
    pub silent: bool,
    /// The tool kind the permission request carries, when present.
    pub tool_kind: Option<String>,
    /// A file the agent appends `session`, `prompt`, and `permission` lines to.
    pub record: Option<PathBuf>,
    /// Stream a thought, a tool call per status, and tool-call updates.
    pub rich: bool,
    /// Offer only an `allow_always` permission option.
    pub allow_always_only: bool,
    /// Stream [`STREAM_SENTENCE`] one character per message chunk.
    pub stream: bool,
    /// Stream a message, a thought, and one tool call, all in single-character chunks.
    pub stream_thoughts: bool,
}

impl Script {
    fn record(
        &self,
        line: &str,
    ) {
        let Some(path) = &self.record else {
            return;
        };
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Runs the scripted agent until the peer closes the channel.
pub fn run_agent(
    link: &mut dyn Link,
    script: &Script,
) {
    loop {
        let Some(received) = link.recv() else {
            return;
        };
        let Ok(message) = serde_json::from_str::<Value>(&received) else {
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                respond(
                    link,
                    &id,
                    &json!({"protocolVersion":1,"agentCapabilities":{}}),
                );
            },
            Some("session/new") => {
                script.record(&format!("session {SESSION_ID}"));
                respond(link, &id, &json!({"sessionId":SESSION_ID}));
            },
            Some("session/prompt") => {
                script.record(&format!("prompt {}", prompt_text(&message)));
                if script.fail {
                    error(link, &id, -32000, "the agent failed on purpose");
                    continue;
                }
                if script.silent {
                    continue;
                }
                stream_opening(link, script);
                if script.rich {
                    stream_rich_updates(link);
                }
                request_permission(link, script);
                respond(link, &id, &json!({"stopReason":"end_turn"}));
            },
            Some("session/cancel") => {
                script.record("cancel");
                if !id.is_null() {
                    respond(link, &id, &json!({}));
                }
            },
            Some(method) if !id.is_null() => {
                error(link, &id, -32601, &format!("{method} is not supported"));
            },
            Some(_) | None => {},
        }
    }
}

fn prompt_text(message: &Value) -> &str {
    message["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

fn stream_opening(
    link: &mut dyn Link,
    script: &Script,
) {
    if script.stream {
        stream_text(link, "agent_message_chunk", STREAM_SENTENCE);
    } else if script.stream_thoughts {
        stream_text(link, "agent_message_chunk", STREAM_SENTENCE);
        stream_text(link, "agent_thought_chunk", STREAM_THOUGHT);
        notify(
            link,
            "session/update",
            &json!({
                "sessionId":SESSION_ID,
                "update":{
                    "sessionUpdate":"tool_call",
                    "toolCallId":"stream-call",
                    "title":"stream tool",
                    "status":"pending"
                }
            }),
        );
    } else {
        notify(
            link,
            "session/update",
            &json!({
                "sessionId":SESSION_ID,
                "update":{
                    "sessionUpdate":"agent_message_chunk",
                    "content":{"type":"text","text":"working"}
                }
            }),
        );
    }
}

fn stream_text(
    link: &mut dyn Link,
    session_update: &str,
    text: &str,
) {
    for character in text.chars() {
        notify(
            link,
            "session/update",
            &json!({
                "sessionId":SESSION_ID,
                "update":{
                    "sessionUpdate":session_update,
                    "content":{"type":"text","text":character.to_string()}
                }
            }),
        );
    }
}

fn stream_rich_updates(link: &mut dyn Link) {
    notify(
        link,
        "session/update",
        &json!({
            "sessionId":SESSION_ID,
            "update":{
                "sessionUpdate":"agent_thought_chunk",
                "content":{"type":"text","text":"considering"}
            }
        }),
    );
    for status in ["pending", "in_progress", "completed", "failed"] {
        notify(
            link,
            "session/update",
            &json!({
                "sessionId":SESSION_ID,
                "update":{
                    "sessionUpdate":"tool_call",
                    "toolCallId":format!("call-{status}"),
                    "title":format!("tool {status}"),
                    "status":status
                }
            }),
        );
    }
    notify(
        link,
        "session/update",
        &json!({
            "sessionId":SESSION_ID,
            "update":{
                "sessionUpdate":"tool_call_update",
                "toolCallId":"call-completed",
                "title":"updated title",
                "status":"completed"
            }
        }),
    );
    notify(
        link,
        "session/update",
        &json!({
            "sessionId":SESSION_ID,
            "update":{
                "sessionUpdate":"tool_call_update",
                "toolCallId":"call-failed",
                "status":"failed"
            }
        }),
    );
    notify(
        link,
        "session/update",
        &json!({
            "sessionId":SESSION_ID,
            "update":{
                "sessionUpdate":"tool_call_update",
                "toolCallId":"call-titled",
                "title":"title only"
            }
        }),
    );
}

fn request_permission(
    link: &mut dyn Link,
    script: &Script,
) {
    let mut tool_call = json!({"toolCallId":"call-1"});
    if let Some(kind) = &script.tool_kind {
        tool_call["kind"] = json!(kind);
    }
    let options = if script.allow_always_only {
        json!([{"optionId":"always","name":"Always","kind":"allow_always"}])
    } else {
        json!([{"optionId":"allow","name":"Allow","kind":"allow_once"}])
    };
    let request = json!({
        "jsonrpc":"2.0",
        "id":100,
        "method":"session/request_permission",
        "params":{
            "sessionId":SESSION_ID,
            "toolCall":tool_call,
            "options":options
        }
    });
    link.send(&request.to_string());
    while let Some(received) = link.recv() {
        let Ok(reply) = serde_json::from_str::<Value>(&received) else {
            continue;
        };
        if reply.get("id").and_then(Value::as_i64) == Some(100) {
            script.record(&format!("permission {}", permission_outcome(&reply)));
            return;
        }
    }
}

fn permission_outcome(reply: &Value) -> &'static str {
    match reply["result"]["outcome"]["outcome"].as_str() {
        Some("selected") => "allow",
        Some("cancelled") => "cancelled",
        _ => "unknown",
    }
}

fn respond(
    link: &mut dyn Link,
    id: &Value,
    result: &Value,
) {
    link.send(&json!({"jsonrpc":"2.0","id":id,"result":result}).to_string());
}

fn notify(
    link: &mut dyn Link,
    method: &str,
    params: &Value,
) {
    link.send(&json!({"jsonrpc":"2.0","method":method,"params":params}).to_string());
}

fn error(
    link: &mut dyn Link,
    id: &Value,
    code: i64,
    message: &str,
) {
    link.send(
        &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string(),
    );
}

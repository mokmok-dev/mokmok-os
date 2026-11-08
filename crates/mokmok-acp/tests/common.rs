//! A scripted ACP agent shared by the integration tests.
//!
//! The agent speaks just enough of the protocol to prove a delegation: it initializes, creates a
//! session, streams one message chunk, asks for permission, and ends the turn.

#![allow(dead_code)]

use std::error::Error;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use mokmok_acp::{
    Handler, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification,
};

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

/// Runs the scripted agent until the peer closes the channel.
pub fn run_agent(link: &mut dyn Link) {
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
                respond(link, &id, &json!({"sessionId":"fake-session"}));
            },
            Some("session/prompt") => {
                notify(
                    link,
                    "session/update",
                    &json!({
                        "sessionId":"fake-session",
                        "update":{
                            "sessionUpdate":"agent_message_chunk",
                            "content":{"type":"text","text":"working"}
                        }
                    }),
                );
                request_permission(link);
                respond(link, &id, &json!({"stopReason":"end_turn"}));
            },
            Some("session/cancel") => {
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

fn request_permission(link: &mut dyn Link) {
    let request = json!({
        "jsonrpc":"2.0",
        "id":100,
        "method":"session/request_permission",
        "params":{
            "sessionId":"fake-session",
            "toolCall":{"toolCallId":"call-1"},
            "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
        }
    });
    link.send(&request.to_string());
    while let Some(received) = link.recv() {
        let Ok(reply) = serde_json::from_str::<Value>(&received) else {
            continue;
        };
        if reply.get("id").and_then(Value::as_i64) == Some(100) {
            return;
        }
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

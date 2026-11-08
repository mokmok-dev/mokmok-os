//! The JSON-RPC envelope and message classification.

use agent_client_protocol_schema::{Error as RpcError, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One decoded JSON-RPC message.
#[derive(Debug)]
pub enum Message {
    Request {
        id: RequestId,
        method: String,
        params: Option<Value>,
    },
    Response {
        id: RequestId,
        result: Option<Value>,
        error: Option<RpcError>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
}

#[derive(Deserialize)]
struct Incoming {
    jsonrpc: Option<String>,
    id: Option<RequestId>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
}

/// Decodes one message, rejecting anything that is not JSON-RPC 2.0.
pub fn parse(line: &str) -> Result<Message, String> {
    let incoming: Incoming = serde_json::from_str(line).map_err(|error| error.to_string())?;
    if incoming.jsonrpc.as_deref() != Some("2.0") {
        return Err("message is not JSON-RPC 2.0".to_owned());
    }
    if let Some(method) = incoming.method {
        return Ok(match incoming.id {
            Some(id) => Message::Request {
                id,
                method,
                params: incoming.params,
            },
            None => Message::Notification {
                method,
                params: incoming.params,
            },
        });
    }
    let Some(id) = incoming.id else {
        return Err("message has neither a method nor an id".to_owned());
    };
    if incoming.result.is_none() && incoming.error.is_none() {
        return Err("response carries neither a result nor an error".to_owned());
    }
    Ok(Message::Response {
        id,
        result: incoming.result,
        error: incoming.error,
    })
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

#[derive(Serialize)]
struct NotificationMessage<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

#[derive(Serialize)]
struct ResponseMessage<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    result: &'a Value,
}

#[derive(Serialize)]
struct ErrorMessage<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    error: &'a RpcError,
}

/// Encodes one request.
pub fn request(
    id: &RequestId,
    method: &str,
    params: Option<&Value>,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&RequestMessage {
        jsonrpc: "2.0",
        id,
        method,
        params,
    })
}

/// Encodes one notification.
pub fn notification(
    method: &str,
    params: Option<&Value>,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&NotificationMessage {
        jsonrpc: "2.0",
        method,
        params,
    })
}

/// Encodes a successful response.
pub fn response(
    id: &RequestId,
    result: &Value,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ResponseMessage {
        jsonrpc: "2.0",
        id,
        result,
    })
}

/// Encodes an error response.
pub fn error_response(
    id: &RequestId,
    error: &RpcError,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ErrorMessage {
        jsonrpc: "2.0",
        id,
        error,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn id() -> RequestId {
        RequestId::Number(7)
    }

    #[test]
    fn a_request_round_trips() {
        let line =
            request(&id(), "initialize", Some(&json!({"protocolVersion": 1}))).expect("encode");
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":1}}"#
        );
        assert!(matches!(
            parse(&line),
            Ok(Message::Request {
                id: RequestId::Number(7),
                method,
                ..
            }) if method == "initialize"
        ));
    }

    #[test]
    fn a_notification_has_no_id() {
        let line =
            notification("session/cancel", Some(&json!({"sessionId": "s"}))).expect("encode");
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}"#
        );
        assert!(
            matches!(parse(&line), Ok(Message::Notification { method, .. }) if method == "session/cancel")
        );
    }

    #[test]
    fn a_result_is_a_response() {
        let line = response(&id(), &json!({"stopReason": "end_turn"})).expect("encode");
        assert!(matches!(
            parse(&line),
            Ok(Message::Response {
                id: RequestId::Number(7),
                result: Some(_),
                error: None,
            })
        ));
    }

    #[test]
    fn an_error_is_a_response() {
        let error = RpcError::method_not_found();
        let line = error_response(&id(), &error).expect("encode");
        let parsed = parse(&line).expect("decode");
        match parsed {
            Message::Response {
                error: Some(error),
                result,
                ..
            } => {
                assert_eq!(error.code, RpcError::method_not_found().code);
                assert!(result.is_none());
            },
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn another_jsonrpc_version_is_refused() {
        assert!(parse(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).is_err());
    }

    #[test]
    fn a_response_without_a_result_or_error_is_refused() {
        assert!(parse(r#"{"jsonrpc":"2.0","id":1}"#).is_err());
    }

    #[test]
    fn a_message_without_a_method_or_id_is_refused() {
        assert!(parse(r#"{"jsonrpc":"2.0"}"#).is_err());
    }

    #[test]
    fn malformed_json_is_refused() {
        assert!(parse("{").is_err());
    }
}

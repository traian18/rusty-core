//! Server-side JSON-RPC message shapes.
//!
//! These are the *mirror images* of the ones in `harness-tool-mcp`: this
//! crate deserializes requests and serializes responses, where the client
//! does the opposite. That opposite polarity is why the two aren't shared —
//! unifying them would mean making every type both `Serialize` and
//! `Deserialize` and owned rather than borrowed, which costs more than the
//! duplication saves.
//!
//! The one value that genuinely must not drift is the protocol version, and
//! that lives in [`harness_protocol::mcp::MCP_PROTOCOL_VERSION`] where both
//! sides read it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC error codes this server produces. Values are from the JSON-RPC
/// 2.0 spec; MCP does not add its own.
pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;

/// One message read from the client. Covers both requests (`id` present)
/// and notifications (`id` absent).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Incoming {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Incoming {
    /// A notification carries no `id` and must not be answered — per
    /// JSON-RPC, replying to one is a protocol error.
    pub(crate) fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    pub(crate) fn params(&self) -> Value {
        self.params.clone().unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ErrorObject {
    pub code: i64,
    pub message: String,
}

impl Response {
    pub(crate) fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_a_notification_are_told_apart_by_id() {
        let request: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert!(!request.is_notification());

        let notification: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(notification.is_notification());
    }

    #[test]
    fn a_success_response_omits_the_error_member_entirely() {
        let value = serde_json::to_value(Response::ok(
            Value::from(1),
            serde_json::json!({"ok": true}),
        ))
        .unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert!(value.as_object().unwrap().get("error").is_none());
    }

    #[test]
    fn an_error_response_omits_the_result_member_entirely() {
        let value = serde_json::to_value(Response::error(Value::from(1), METHOD_NOT_FOUND, "nope"))
            .unwrap();
        assert!(value.as_object().unwrap().get("result").is_none());
        assert_eq!(value["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn missing_params_read_as_null_rather_than_failing() {
        let request: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(request.params(), Value::Null);
    }
}

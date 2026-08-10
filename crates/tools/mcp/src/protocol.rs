//! Minimal MCP (Model Context Protocol) JSON-RPC message shapes.
//!
//! Deliberately not a full MCP SDK: this crate only speaks the subset
//! needed to connect, discover `tools/list`, and drive `tools/call` — the
//! parts a tool-using agent harness needs. Fields the harness doesn't act
//! on (server capabilities, logging, sampling, roots, resources, prompts)
//! are left untyped or dropped on the floor rather than modeled.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version this client claims during `initialize`. MCP servers
/// negotiate down to a version they support; a server's response is
/// accepted regardless of what it echoes back — see the note on
/// [`super::client::McpClient::connect`].
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcNotification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A line read back from the server: either a response to one of our
/// requests (`id` + `result`/`error`) or a request/notification the server
/// initiated (`method` present). This client only handles the former; see
/// [`super::client::read_loop`].
#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcInbound {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcErrorObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InitializeResult {
    #[serde(default, rename = "serverInfo")]
    pub server_info: Option<ServerInfo>,
}

/// Identifies the server this client connected to, as reported during
/// `initialize`.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// One entry from a server's `tools/list` response.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListToolsResult {
    #[serde(default)]
    pub tools: Vec<McpToolInfo>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_omits_params_when_none_and_uses_protocol_field_names() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "tools/list",
            params: None,
        };
        let value: Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert_eq!(value["method"], "tools/list");
        assert!(
            value.as_object().unwrap().get("params").is_none(),
            "params must be omitted, not sent as null, when unset"
        );
    }

    #[test]
    fn notification_serializes_without_an_id_field() {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized",
            params: None,
        };
        let value: Value = serde_json::to_value(&notification).unwrap();
        assert!(value.as_object().unwrap().get("id").is_none());
    }

    #[test]
    fn inbound_response_and_inbound_server_request_are_told_apart_by_method() {
        // A reply to one of our requests: `id` + `result`, no `method`.
        let reply: JsonRpcInbound =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert!(reply.method.is_none());
        assert_eq!(reply.id, Some(Value::from(1)));

        // A request/notification the server initiated: `method` present.
        let server_initiated: JsonRpcInbound =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"sampling/createMessage"}"#).unwrap();
        assert_eq!(
            server_initiated.method.as_deref(),
            Some("sampling/createMessage")
        );
    }

    #[test]
    fn list_tools_result_deserializes_a_realistic_server_payload() {
        let json = r#"{
            "tools": [
                {
                    "name": "read_file",
                    "description": "Reads a file",
                    "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            ],
            "nextCursor": "page-2"
        }"#;
        let result: ListToolsResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "read_file");
        assert_eq!(result.tools[0].description.as_deref(), Some("Reads a file"));
        assert_eq!(result.next_cursor.as_deref(), Some("page-2"));
    }

    #[test]
    fn list_tools_result_defaults_missing_optional_fields() {
        let result: ListToolsResult = serde_json::from_str("{}").unwrap();
        assert!(result.tools.is_empty());
        assert_eq!(result.next_cursor, None);
    }

    #[test]
    fn call_tool_result_defaults_is_error_to_false_when_absent() {
        let result: CallToolResult =
            serde_json::from_str(r#"{"content":[{"type":"text","text":"hi"}]}"#).unwrap();
        assert_eq!(result.is_error, None);
        assert_eq!(result.content.len(), 1);
    }
}

/// Result of a `tools/call`. `content` is kept as raw JSON blocks (`text`,
/// `image`, `resource`, ...) rather than a typed enum, so a future content
/// type the server sends doesn't fail deserialization — see
/// `call_result_to_output` (private to `crate::tool`) for how this becomes
/// a `ToolResult`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default, rename = "isError")]
    pub is_error: Option<bool>,
}

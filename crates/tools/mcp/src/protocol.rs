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

/// Result of a `tools/call`. `content` is kept as raw JSON blocks (`text`,
/// `image`, `resource`, ...) rather than a typed enum, so a future content
/// type the server sends doesn't fail deserialization — see
/// [`super::tool::call_result_to_output`] for how this becomes a
/// `ToolResult`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default, rename = "isError")]
    pub is_error: Option<bool>,
}

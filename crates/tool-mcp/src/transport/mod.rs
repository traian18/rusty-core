//! The seam between "what MCP methods mean" and "how bytes get to the
//! server".
//!
//! The trait is cut at `request`/`notify` — whole JSON-RPC calls — rather
//! than at raw send/receive, because the two transports correlate replies
//! completely differently:
//!
//! - **stdio** multiplexes every request and reply over one pipe, so it
//!   needs an id counter and a pending-request map to route a reply back to
//!   the caller that is waiting for it.
//! - **streamable HTTP** answers each POST on that POST's own response, so
//!   correlation is structural and a pending map would be dead weight.
//!
//! Cutting lower would have forced the HTTP transport to fake a framing
//! model it doesn't have. Everything above this trait — `initialize`,
//! `tools/list` pagination, `tools/call`, the namespacing in
//! [`crate::tool`] — is transport-agnostic and written once.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::McpError;

pub(crate) mod http;
pub(crate) mod stdio;

#[async_trait]
pub(crate) trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and wait for its result.
    ///
    /// Returns the `result` member on success, or [`McpError::Rpc`] when
    /// the server answered with an `error` member.
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError>;

    /// Send a JSON-RPC notification. No reply is expected or awaited.
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError>;

    /// Release whatever the transport holds — a child process, an HTTP
    /// session. Must be safe to call more than once.
    async fn shutdown(&self);
}

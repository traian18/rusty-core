//! [`McpClient`]: the MCP method surface, over whichever transport the
//! config selected.
//!
//! Everything here is transport-agnostic — the handshake, `tools/list`
//! pagination, and `tools/call` are written once and work identically over
//! a spawned process or an HTTPS endpoint. See [`crate::transport`] for why
//! the seam sits at whole JSON-RPC calls rather than at raw bytes.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{McpServerConfig, McpTransportConfig};
use crate::error::McpError;
use crate::protocol::{
    CallToolResult, InitializeResult, ListToolsResult, McpToolInfo, ServerInfo,
    MCP_PROTOCOL_VERSION,
};
use crate::transport::{http::HttpTransport, stdio::StdioTransport, McpTransport};

const CLIENT_NAME: &str = "rusty-core";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A live connection to one MCP server.
///
/// Cheap to clone via `Arc` — every [`crate::tool::McpToolExecutor`]
/// discovered from the same server shares one `McpClient`, so a server
/// with ten tools spawns one process (or opens one HTTP session), not ten.
pub struct McpClient {
    transport: Arc<dyn McpTransport>,
    server_info: OnceLock<ServerInfo>,
}

impl McpClient {
    /// Connect using `config`'s transport, drive the `initialize`
    /// handshake, and send `notifications/initialized`. The returned client
    /// is ready for `list_tools`/`call_tool`.
    pub async fn connect(config: &McpServerConfig) -> Result<Arc<Self>, McpError> {
        let request_timeout = config.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);

        let transport: Arc<dyn McpTransport> = match &config.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => Arc::new(
                StdioTransport::connect(
                    &config.name,
                    command,
                    args,
                    env,
                    cwd.as_deref(),
                    request_timeout,
                )
                .await?,
            ),
            McpTransportConfig::Http { url, headers } => Arc::new(HttpTransport::connect(
                &config.name,
                url,
                headers,
                request_timeout,
            )?),
        };

        let client = Arc::new(Self {
            transport,
            server_info: OnceLock::new(),
        });

        client.initialize().await?;
        Ok(client)
    }

    /// The server's self-reported name/version, if it sent one during
    /// `initialize`.
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.get()
    }

    async fn initialize(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION },
        });
        let result = self.transport.request("initialize", Some(params)).await?;
        let result: InitializeResult = serde_json::from_value(result)?;
        if let Some(info) = result.server_info {
            let _ = self.server_info.set(info);
        }
        self.transport
            .notify("notifications/initialized", None)
            .await
    }

    /// List every tool the server advertises, following `nextCursor`
    /// pagination until exhausted.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
            let result = self.transport.request("tools/list", params).await?;
            let result: ListToolsResult = serde_json::from_value(result)?;
            tools.extend(result.tools);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Invoke a remote tool by its server-local name.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.transport.request("tools/call", Some(params)).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Release the transport — kill the child process, or terminate the
    /// HTTP session. Safe to call more than once.
    pub async fn shutdown(&self) {
        self.transport.shutdown().await;
    }
}

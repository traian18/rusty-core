#![warn(clippy::all)]

//! MCP (Model Context Protocol) client tools.
//!
//! Connects to external MCP servers over stdio and exposes each tool they
//! advertise as a [`ToolExecutor`](harness_tools::ToolExecutor), so any MCP
//! server (filesystem, GitHub, Slack, a database, ...) becomes callable by
//! an agent the same way a built-in tool is — no protocol/runtime changes
//! needed, since `ToolExecutor` is already the seam every tool in this
//! workspace goes through.
//!
//! # Scope
//!
//! **Transports**: stdio (spawn a process) and streamable HTTP (POST to an
//! endpoint, reply as JSON or SSE). Both sit behind one internal trait, so
//! everything above `McpClient` is written once — see [`transport`].
//!
//! **Methods**: `tools/*` only — `initialize` →
//! `notifications/initialized` → `tools/list` → `tools/call`. Resources,
//! prompts, sampling, and roots aren't implemented; nothing here precludes
//! adding them, they just aren't needed for "give the agent more tools".
//!
//! # Usage
//!
//! ```no_run
//! # async fn example() -> Result<(), harness_tool_mcp::McpError> {
//! use harness_tool_mcp::{connect_and_discover, McpServerConfig};
//!
//! // A local server, spawned over stdio:
//! let local = McpServerConfig::new("filesystem", "npx")
//!     .args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]);
//!
//! // A hosted one, over HTTP:
//! let remote = McpServerConfig::http("remote", "https://example.com/mcp")
//!     .header("Authorization", "Bearer ...");
//!
//! let tools = connect_and_discover(&local).await?;
//! // Register each into a ToolRegistry alongside the built-in tools.
//! # let _ = remote;
//! # Ok(())
//! # }
//! ```

mod client;
mod config;
mod error;
mod protocol;
mod tool;
mod transport;

pub use client::McpClient;
pub use config::{McpServerConfig, McpTransportConfig};
pub use error::McpError;
pub use protocol::{CallToolResult, McpToolInfo, ServerInfo};
pub use tool::{connect_and_discover, McpToolExecutor};

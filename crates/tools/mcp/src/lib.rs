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
//! v1 covers the **stdio transport** and **tools** only:
//! `initialize` → `notifications/initialized` → `tools/list` →
//! `tools/call`. Resources, prompts, sampling, roots, and the
//! HTTP/streamable-HTTP transports aren't implemented — nothing here
//! precludes adding them later, they just aren't needed for "give the
//! agent more tools" yet.
//!
//! # Usage
//!
//! ```no_run
//! # async fn example() -> Result<(), harness_tool_mcp::McpError> {
//! use harness_tool_mcp::{connect_and_discover, McpServerConfig};
//!
//! let config = McpServerConfig::new("filesystem", "npx")
//!     .args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]);
//! let tools = connect_and_discover(&config).await?;
//! // Register each into a ToolRegistry alongside the built-in tools.
//! # Ok(())
//! # }
//! ```

mod client;
mod config;
mod protocol;
mod tool;

pub use client::{McpClient, McpError};
pub use config::McpServerConfig;
pub use protocol::{CallToolResult, McpToolInfo, ServerInfo};
pub use tool::{connect_and_discover, McpToolExecutor};

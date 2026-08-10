//! Wire-serializable MCP server launch spec.
//!
//! `harness-tool-mcp` owns the real `McpServerConfig` type and the client
//! that actually spawns/talks to a server — but that's an I/O-bearing
//! `harness-tool-*` crate, and this crate must stay dependency-direction
//! clean (no runtime, no I/O; see `xtask check-deps`), so it can't reuse
//! that type directly. [`McpServerSpec`] is a plain, serializable mirror of
//! the same fields, which is all `RpcRequestBody::CreateSession` needs to
//! carry an MCP server launch request over the wire. Whatever host actually
//! creates the session (e.g. `apps/harnessd`'s handler) converts this into
//! a real `harness_tool_mcp::McpServerConfig` before connecting.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Launch spec for one MCP server, connected over stdio when the session
/// that requests it starts. Field-for-field mirror of
/// `harness_tool_mcp::McpServerConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerSpec {
    /// Short, unique name for this server. Namespaces the tool ids it
    /// contributes (`mcp.<name>.<tool>`) and appears in diagnostics.
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables merged into the child's environment.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory for the child process. `None` inherits the
    /// harness process's own cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Per-request timeout in seconds (`initialize`, `tools/list`,
    /// `tools/call`). `None` uses the client's own default (60s).
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_with_only_required_fields() {
        let spec = McpServerSpec {
            name: "filesystem".to_owned(),
            command: "npx".to_owned(),
            args: vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
            ],
            env: HashMap::new(),
            cwd: None,
            request_timeout_secs: None,
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let back: McpServerSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    #[test]
    fn deserializes_with_only_name_and_command_present() {
        let json = r#"{"name":"filesystem","command":"npx"}"#;
        let spec: McpServerSpec = serde_json::from_str(json).expect("deserialize");
        assert_eq!(spec.name, "filesystem");
        assert_eq!(spec.command, "npx");
        assert!(spec.args.is_empty());
        assert!(spec.env.is_empty());
        assert_eq!(spec.cwd, None);
        assert_eq!(spec.request_timeout_secs, None);
    }
}

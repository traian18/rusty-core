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

/// Connection spec for one MCP server, connected when the session that
/// requests it starts. Mirror of `harness_tool_mcp::McpServerConfig`.
///
/// # Wire compatibility
///
/// The flat `command`/`args`/`env`/`cwd` fields are the original v2 shape,
/// from when stdio was the only transport. [`transport`](Self::transport)
/// was added on top rather than replacing them, so:
///
/// - `transport: None` (or absent) — read the flat fields as a stdio
///   server. This is exactly what a pre-existing client sends, so it keeps
///   working without a `PROTOCOL_VERSION` bump.
/// - `transport: Some(_)` — the flat fields are ignored.
///
/// Use [`resolve_transport`](Self::resolve_transport) rather than reading
/// either representation directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerSpec {
    /// Short, unique name for this server. Namespaces the tool ids it
    /// contributes (`mcp.<name>.<tool>`) and appears in diagnostics.
    pub name: String,
    /// How to reach the server. When `None`, the flat legacy fields below
    /// describe a stdio server.
    #[serde(default)]
    pub transport: Option<McpTransportSpec>,
    /// Executable to spawn. Legacy stdio field; ignored when
    /// [`transport`](Self::transport) is set.
    #[serde(default)]
    pub command: String,
    /// Arguments passed to `command`. Legacy stdio field.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables merged into the child's environment.
    /// Legacy stdio field.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory for the child process. `None` inherits the
    /// harness process's own cwd. Legacy stdio field.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Per-request timeout in seconds (`initialize`, `tools/list`,
    /// `tools/call`). `None` uses the client's own default (60s).
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
}

/// How to reach an MCP server. Mirror of
/// `harness_tool_mcp::McpTransportConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransportSpec {
    /// Spawn a local process and speak newline-delimited JSON over its
    /// stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    /// POST JSON-RPC to a remote endpoint ("streamable HTTP").
    Http {
        url: String,
        /// Extra request headers. This is where an
        /// `Authorization: Bearer ...` goes.
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl McpServerSpec {
    /// Create a stdio spec.
    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: Some(McpTransportSpec::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
            }),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            request_timeout_secs: None,
        }
    }

    /// Create an HTTP spec.
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: Some(McpTransportSpec::Http {
                url: url.into(),
                headers: HashMap::new(),
            }),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            request_timeout_secs: None,
        }
    }

    /// The transport this spec describes, whichever representation the
    /// sender used.
    ///
    /// Callers should always go through this instead of reading
    /// [`transport`](Self::transport) or the flat fields, so the legacy
    /// shape stays handled in exactly one place.
    pub fn resolve_transport(&self) -> McpTransportSpec {
        self.transport
            .clone()
            .unwrap_or_else(|| McpTransportSpec::Stdio {
                command: self.command.clone(),
                args: self.args.clone(),
                env: self.env.clone(),
                cwd: self.cwd.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_with_only_required_fields() {
        let spec = McpServerSpec {
            name: "filesystem".to_owned(),
            transport: None,
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

    /// The compatibility guarantee: a payload in the original v2 shape,
    /// with no `transport` member at all, must still resolve to the stdio
    /// server it describes. This is what lets the field be additive rather
    /// than a protocol version bump.
    #[test]
    fn a_legacy_payload_without_a_transport_resolves_to_stdio() {
        let json = r#"{"name":"filesystem","command":"npx","args":["-y","server"],"cwd":"/srv"}"#;
        let spec: McpServerSpec = serde_json::from_str(json).expect("deserialize");
        assert_eq!(spec.transport, None);

        let McpTransportSpec::Stdio {
            command, args, cwd, ..
        } = spec.resolve_transport()
        else {
            panic!("a legacy payload must resolve to stdio");
        };
        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y".to_owned(), "server".to_owned()]);
        assert_eq!(cwd.as_deref(), Some("/srv"));
    }

    #[test]
    fn an_http_transport_round_trips_and_resolves() {
        let json = r#"{
            "name": "remote",
            "transport": {
                "kind": "http",
                "url": "https://example.com/mcp",
                "headers": {"Authorization": "Bearer t"}
            }
        }"#;
        let spec: McpServerSpec = serde_json::from_str(json).expect("deserialize");

        let McpTransportSpec::Http { url, headers } = spec.resolve_transport() else {
            panic!("expected an http transport");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer t")
        );

        let round_tripped: McpServerSpec =
            serde_json::from_str(&serde_json::to_string(&spec).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_tripped, spec);
    }

    /// An explicit `transport` wins; the flat fields are inert once it is
    /// present, so a client that sets both can't get a surprising hybrid.
    #[test]
    fn an_explicit_transport_takes_precedence_over_the_legacy_fields() {
        let json = r#"{
            "name": "remote",
            "command": "should-be-ignored",
            "transport": {"kind": "http", "url": "https://example.com/mcp"}
        }"#;
        let spec: McpServerSpec = serde_json::from_str(json).expect("deserialize");
        assert!(matches!(
            spec.resolve_transport(),
            McpTransportSpec::Http { .. }
        ));
    }

    #[test]
    fn the_constructors_produce_resolvable_specs() {
        assert!(matches!(
            McpServerSpec::stdio("local", "npx").resolve_transport(),
            McpTransportSpec::Stdio { .. }
        ));
        assert!(matches!(
            McpServerSpec::http("remote", "https://example.com/mcp").resolve_transport(),
            McpTransportSpec::Http { .. }
        ));
    }
}

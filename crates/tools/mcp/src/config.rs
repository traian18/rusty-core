use std::collections::HashMap;
use std::time::Duration;

/// How to reach one MCP server.
///
/// Both variants end up behind the same `McpTransport` trait, so everything
/// above `McpClient` — tool discovery, namespacing, the `ToolExecutor`
/// wrapper, the engine's registration — is identical either way.
#[derive(Debug, Clone)]
pub enum McpTransportConfig {
    /// Spawn a local process and speak newline-delimited JSON over its
    /// stdin/stdout.
    Stdio {
        /// Executable to spawn.
        command: String,
        /// Arguments passed to `command`.
        args: Vec<String>,
        /// Extra environment variables merged into the child's environment.
        env: HashMap<String, String>,
        /// Working directory for the child process. Defaults to the harness
        /// process's own cwd when unset.
        cwd: Option<String>,
    },
    /// POST JSON-RPC to a remote endpoint (MCP's "streamable HTTP"
    /// transport), accepting either a plain JSON reply or an SSE stream.
    Http {
        /// Full endpoint URL, e.g. `https://example.com/mcp`.
        url: String,
        /// Extra request headers, merged into every request. This is where
        /// an `Authorization: Bearer ...` goes.
        ///
        /// Held in memory as plain `String`s, the same as integration API
        /// keys elsewhere in the workspace.
        headers: HashMap<String, String>,
    },
}

/// Launch/connection configuration for one MCP server.
///
/// The stdio shape mirrors what most MCP-capable clients already use for
/// their `mcpServers` config (Claude Desktop, Claude Code, Cursor, ...): a
/// command, its arguments, and optional environment/cwd overrides — so
/// entries from an existing config file can be translated field-for-field.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Short, unique name for this server. Used to namespace the tool ids
    /// it contributes (`mcp.<name>.<tool>`) and in diagnostics.
    pub name: String,
    /// How to reach it.
    pub transport: McpTransportConfig,
    /// Per-request timeout (`initialize`, `tools/list`, `tools/call`).
    /// Defaults to 60s when unset.
    pub request_timeout: Option<Duration>,
}

impl McpServerConfig {
    /// Create a **stdio** server config with no arguments/env/cwd set.
    ///
    /// Kept at this exact signature — rather than taking an
    /// [`McpTransportConfig`] — because it is the overwhelmingly common
    /// case and every existing caller uses it.
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransportConfig::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
            },
            request_timeout: None,
        }
    }

    /// Create an **HTTP** server config pointing at `url`.
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransportConfig::Http {
                url: url.into(),
                headers: HashMap::new(),
            },
            request_timeout: None,
        }
    }

    /// Append a single argument. **No-op on an HTTP config.**
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        if let McpTransportConfig::Stdio { args, .. } = &mut self.transport {
            args.push(arg.into());
        }
        self
    }

    /// Append multiple arguments. **No-op on an HTTP config.**
    pub fn args(mut self, new_args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        if let McpTransportConfig::Stdio { args, .. } = &mut self.transport {
            args.extend(new_args.into_iter().map(Into::into));
        }
        self
    }

    /// Set an environment variable for the child process. **No-op on an
    /// HTTP config** — use [`header`](Self::header) there.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if let McpTransportConfig::Stdio { env, .. } = &mut self.transport {
            env.insert(key.into(), value.into());
        }
        self
    }

    /// Set the child process's working directory. **No-op on an HTTP
    /// config.**
    pub fn cwd(mut self, new_cwd: impl Into<String>) -> Self {
        if let McpTransportConfig::Stdio { cwd, .. } = &mut self.transport {
            *cwd = Some(new_cwd.into());
        }
        self
    }

    /// Set a request header. **No-op on a stdio config** — use
    /// [`env`](Self::env) there.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if let McpTransportConfig::Http { headers, .. } = &mut self.transport {
            headers.insert(key.into(), value.into());
        }
        self
    }

    /// Override the default 60s per-request timeout.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_builds_a_stdio_config_and_the_stdio_builders_apply() {
        let config = McpServerConfig::new("filesystem", "npx")
            .args(["-y", "@modelcontextprotocol/server-filesystem"])
            .arg("/tmp")
            .env("NODE_ENV", "production")
            .cwd("/srv");

        let McpTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } = &config.transport
        else {
            panic!("expected a stdio config");
        };
        assert_eq!(command, "npx");
        assert_eq!(
            args,
            &["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert_eq!(env.get("NODE_ENV").map(String::as_str), Some("production"));
        assert_eq!(cwd.as_deref(), Some("/srv"));
    }

    #[test]
    fn http_builds_an_http_config_and_headers_apply() {
        let config = McpServerConfig::http("remote", "https://example.com/mcp")
            .header("Authorization", "Bearer token");

        let McpTransportConfig::Http { url, headers } = &config.transport else {
            panic!("expected an http config");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
    }

    /// The builders are documented as no-ops across transports rather than
    /// panicking, so a caller threading one config shape through generic
    /// code can't blow up. Pin that they really are inert.
    #[test]
    fn cross_transport_builders_are_inert_rather_than_panicking() {
        let http = McpServerConfig::http("remote", "https://example.com/mcp")
            .arg("ignored")
            .env("ALSO", "ignored")
            .cwd("/ignored");
        let McpTransportConfig::Http { url, headers } = &http.transport else {
            panic!("expected an http config");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert!(headers.is_empty());

        let stdio = McpServerConfig::new("local", "npx").header("Authorization", "ignored");
        let McpTransportConfig::Stdio { args, env, .. } = &stdio.transport else {
            panic!("expected a stdio config");
        };
        assert!(args.is_empty());
        assert!(env.is_empty());
    }
}

use std::collections::HashMap;
use std::time::Duration;

/// Launch configuration for one MCP server, spoken over stdio.
///
/// Mirrors the shape most MCP-capable clients already use for their
/// `mcpServers` config (Claude Desktop, Claude Code, Cursor, ...): a
/// command, its arguments, and optional environment/cwd overrides — so
/// entries from an existing config file can be translated into this
/// struct field-for-field.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Short, unique name for this server. Used to namespace the tool ids
    /// it contributes (`mcp.<name>.<tool>`) and in diagnostics.
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to `command`.
    pub args: Vec<String>,
    /// Extra environment variables merged into the child's environment.
    pub env: HashMap<String, String>,
    /// Working directory for the child process. Defaults to the harness
    /// process's own cwd when unset.
    pub cwd: Option<String>,
    /// Per-request timeout (`initialize`, `tools/list`, `tools/call`).
    /// Defaults to 60s when unset.
    pub request_timeout: Option<Duration>,
}

impl McpServerConfig {
    /// Create a server config with no arguments/env/cwd set.
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            request_timeout: None,
        }
    }

    /// Append a single argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable for the child process.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the child process's working directory.
    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Override the default 60s per-request timeout.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }
}

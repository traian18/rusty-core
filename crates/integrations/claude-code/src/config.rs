use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for the Claude Code CLI integration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeCodeConfig {
    /// Path to the Claude Code CLI binary (default: resolve "claude" via $PATH).
    #[serde(default = "default_binary_path")]
    pub binary_path: PathBuf,

    /// Additional command-line arguments to pass to the CLI.
    /// Common examples: --model sonnet
    #[serde(default)]
    pub extra_args: Vec<String>,

    /// Permission mode for the CLI. The harness is the single permission
    /// layer, so non-interactive runs default to `bypassPermissions` — one of
    /// the Claude CLI's valid modes (`default`, `acceptEdits`, `plan`,
    /// `bypassPermissions`). The sentinel `"interactive"` omits the flag
    /// entirely and lets the CLI prompt.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,

    /// Optional timeout in seconds for the CLI subprocess.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_binary_path() -> PathBuf {
    "claude".into()
}

fn default_permission_mode() -> String {
    "bypassPermissions".to_string()
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            binary_path: default_binary_path(),
            extra_args: Vec::new(),
            permission_mode: default_permission_mode(),
            timeout_secs: None,
        }
    }
}

impl ClaudeCodeConfig {
    /// Create a new config with the default binary path ("claude").
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a custom binary path.
    pub fn with_binary_path(mut self, path: PathBuf) -> Self {
        self.binary_path = path;
        self
    }

    /// Add an extra CLI argument.
    pub fn with_arg(mut self, arg: String) -> Self {
        self.extra_args.push(arg);
        self
    }

    /// Set the permission mode.
    pub fn with_permission_mode(mut self, mode: String) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Set a timeout in seconds.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

use std::path::PathBuf;

/// Configuration for driving the Claude Code CLI as a subprocess backend.
///
/// # `permission_mode` and trust
///
/// Defaults to `"bypassPermissions"` — the harness process has no TTY to
/// relay the CLI's own interactive permission prompts to, so the delegated
/// Claude Code instance must run autonomously using its own tools
/// (Read/Write/Edit/Bash/...) within `working_dir`. This is a deliberate,
/// documented trust decision (see `crates/integrations/claude-code/PLAN.md`,
/// question 1): the harness's own tool registry and permission system are
/// bypassed entirely for this backend — the CLI manages its own tools
/// end-to-end. Only use this integration where that's the intended
/// delegation model, and scope `working_dir` accordingly.
#[derive(Clone, Debug)]
pub struct ClaudeCodeConfig {
    /// Resolved via `$PATH` by default.
    pub binary_path: PathBuf,
    /// Extra CLI arguments passed through verbatim (e.g. `["--model", "opus"]`).
    pub extra_args: Vec<String>,
    /// One of `"bypassPermissions"`, `"acceptEdits"`, `"plan"`, etc. — see
    /// `claude --help`'s `--permission-mode` for the current set.
    pub permission_mode: String,
    /// Working directory for the spawned process. `None` inherits the
    /// harness process's own current directory.
    pub working_dir: Option<PathBuf>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("claude"),
            extra_args: Vec::new(),
            permission_mode: "bypassPermissions".to_string(),
            working_dir: None,
        }
    }
}

impl ClaudeCodeConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

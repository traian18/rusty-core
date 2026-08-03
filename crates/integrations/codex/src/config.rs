use std::path::PathBuf;

/// Configuration for driving the Codex CLI (`@openai/codex`) as a
/// subprocess backend.
///
/// # `sandbox_mode` and trust
///
/// Defaults to `"workspace-write"` — Codex's `exec` mode has a graduated
/// sandbox policy (`read-only`, `workspace-write`, `danger-full-access`)
/// rather than Claude Code's single permission-mode switch, so unlike
/// `ClaudeCodeConfig` (which defaults to fully bypassing permissions because
/// there's no other non-interactive option), this can default to a
/// narrower, still-autonomous policy. Set `dangerously_bypass: true` (mapping
/// to `--dangerously-bypass-approvals-and-sandbox`) only where the process
/// is already externally sandboxed — see Codex's own help text for that
/// flag. Same trust boundary as Claude Code applies either way: this
/// backend's tool execution is not managed by the harness host at all (see
/// `backend.rs`'s `host_managed_tools: false`).
#[derive(Clone, Debug)]
pub struct CodexConfig {
    /// Resolved via `$PATH` by default.
    pub binary_path: PathBuf,
    /// Extra CLI arguments passed through verbatim (e.g. `["--model", "o3"]`).
    pub extra_args: Vec<String>,
    /// One of `"read-only"`, `"workspace-write"`, `"danger-full-access"`.
    /// Only applied on a *fresh* session — `codex exec resume` does not
    /// accept `--sandbox` (verified against CLI 0.146.0; it inherits the
    /// original session's policy).
    pub sandbox_mode: String,
    /// Maps to `--dangerously-bypass-approvals-and-sandbox`.
    pub dangerously_bypass: bool,
    /// Working directory for the spawned process, passed via `-C` on a
    /// fresh session only (`resume` doesn't accept `-C` either). `None`
    /// inherits the harness process's own current directory.
    pub working_dir: Option<PathBuf>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("codex"),
            extra_args: Vec::new(),
            sandbox_mode: "workspace-write".to_string(),
            dangerously_bypass: false,
            working_dir: None,
        }
    }
}

impl CodexConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

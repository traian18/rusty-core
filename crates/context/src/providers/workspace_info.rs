//! A short listing of the workspace's files, added to the system prompt.

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;

use rusty_runtime::traits::Workspace;

use crate::provider::ContextProvider;

/// Prepends a fixed instruction string to the system prompt — the

/// Appends a short workspace summary (root path + a shallow file listing) to
/// the system prompt so the agent has basic orientation without spending a
/// tool round-trip on "where am I."
pub struct WorkspaceInfoProvider {
    max_files: usize,
}

impl WorkspaceInfoProvider {
    pub fn new(max_files: usize) -> Self {
        Self { max_files }
    }
}

impl Default for WorkspaceInfoProvider {
    /// 50 entries keeps the summary compact for typical projects without
    /// needing tuning; pass an explicit value via [`Self::new`] for larger
    /// workspaces.
    fn default() -> Self {
        Self::new(50)
    }
}

#[async_trait]
impl ContextProvider for WorkspaceInfoProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let root = workspace.root().display().to_string();
        let summary = match workspace.list_files(1).await {
            Ok(files) => {
                let overflow = files.len().saturating_sub(self.max_files);
                let mut lines: Vec<String> = files
                    .iter()
                    .take(self.max_files)
                    .map(|file| file.path.display().to_string())
                    .collect();
                if overflow > 0 {
                    lines.push(format!("... ({overflow} more)"));
                }
                if lines.is_empty() {
                    format!("Workspace root: {root}")
                } else {
                    format!(
                        "Workspace root: {root}\nTop-level entries:\n{}",
                        lines.join("\n")
                    )
                }
            }
            // A workspace that can't list files (e.g. a restricted or
            // not-yet-populated one) still gets a usable prompt — just
            // without the listing, rather than failing the whole request.
            Err(_) => format!("Workspace root: {root}"),
        };

        request.system_prompt = if request.system_prompt.is_empty() {
            summary
        } else {
            format!("{}\n\n{summary}", request.system_prompt)
        };
        request
    }
}

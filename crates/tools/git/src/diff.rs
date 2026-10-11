use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

use crate::patch::{apply_pathspecs, render, DiffFilters};

/// Input for the `git.diff` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitDiffInput {
    /// Optional path filter, relative to the repo root.
    #[serde(default)]
    pub path: Option<String>,
    /// `false` (default): working tree vs index. `true`: index vs HEAD.
    #[serde(default)]
    pub staged: bool,
    #[serde(flatten)]
    pub filters: DiffFilters,
}

/// Shows a diff for a path or the whole tree. Read-only.
pub struct GitDiffTool {
    repo_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

#[async_trait]
impl ToolExecutor for GitDiffTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(GitDiffInput);
        ToolDescriptor {
            id: ToolId::new("git.diff"),
            name: "Git diff".to_string(),
            description: "Show a diff for a path or the whole tree (working tree or staged). \
                          Returns per-file change counts plus the patch, limited by file count and size; \
                          narrow with paths, hunk_contains, max_hunks_per_file, or use summary_only."
                .to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: GitDiffInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let repo_root = self.repo_root.clone();
        let result = tokio::task::spawn_blocking(move || run_diff(&repo_root, &input))
            .await
            .map_err(|_| ToolError::Internal)?;

        match result {
            Ok(fields) => Ok(ToolResult {
                call_id: "git.diff".to_string(),
                output: serde_json::Value::Object(fields),
                is_error: false,
            }),
            Err(message) => Ok(ToolResult {
                call_id: "git.diff".to_string(),
                output: json!({ "error": message }),
                is_error: true,
            }),
        }
    }
}

fn run_diff(
    repo_root: &Path,
    input: &GitDiffInput,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;

    let mut options = git2::DiffOptions::new();
    apply_pathspecs(
        &mut options,
        input
            .path
            .iter()
            .chain(&input.filters.paths)
            .map(String::as_str),
    );

    let diff = if input.staged {
        let head_tree = repo
            .head()
            .and_then(|head| head.peel_to_tree())
            .map_err(|e| e.to_string())?;
        repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut options))
            .map_err(|e| e.to_string())?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut options))
            .map_err(|e| e.to_string())?
    };

    render(&diff, &input.filters, true)
}

#[cfg(test)]
mod tests;

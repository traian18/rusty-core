use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

/// Input for the `git.status` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitStatusInput {
    /// Optional path filter, relative to the repo root.
    #[serde(default)]
    pub path: Option<String>,
}

/// Reports working-tree and index status for changed files. Read-only — see
/// `crates/tools/git/PLAN.md` for why this crate has no mutating tools.
pub struct GitStatusTool {
    repo_root: PathBuf,
}

impl GitStatusTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

#[async_trait]
impl ToolExecutor for GitStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(GitStatusInput);
        ToolDescriptor {
            id: ToolId::new("git.status"),
            name: "Git status".to_string(),
            description: "Show working-tree and index status for changed files".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: GitStatusInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let repo_root = self.repo_root.clone();
        // git2 does blocking filesystem I/O and its `Repository` is not
        // constructed for use across an `.await` — the whole operation runs
        // on a blocking thread and only plain data crosses back.
        let result = tokio::task::spawn_blocking(move || run_status(&repo_root, input.path.as_deref()))
            .await
            .map_err(|_| ToolError::Internal)?;

        match result {
            Ok(entries) => Ok(ToolResult {
                call_id: "git.status".to_string(),
                output: json!({ "entries": entries }),
                is_error: false,
            }),
            Err(message) => Ok(ToolResult {
                call_id: "git.status".to_string(),
                output: json!({ "error": message }),
                is_error: true,
            }),
        }
    }
}

fn run_status(repo_root: &Path, path_filter: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;

    let mut options = git2::StatusOptions::new();
    options.include_untracked(true);
    if let Some(path) = path_filter {
        options.pathspec(path);
    }

    let statuses = repo.statuses(Some(&mut options)).map_err(|e| e.to_string())?;
    let entries = statuses
        .iter()
        .map(|entry| {
            let status = entry.status();
            json!({
                "path": entry.path().unwrap_or("").to_string(),
                "index_new": status.is_index_new(),
                "index_modified": status.is_index_modified(),
                "index_deleted": status.is_index_deleted(),
                "index_renamed": status.is_index_renamed(),
                "wt_new": status.is_wt_new(),
                "wt_modified": status.is_wt_modified(),
                "wt_deleted": status.is_wt_deleted(),
                "conflicted": status.is_conflicted(),
            })
        })
        .collect();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo_with_commit(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).expect("init repo");
        std::fs::write(dir.join("a.txt"), "hello\n").expect("write file");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.txt")).expect("add path");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
        {
            let tree = repo.find_tree(tree_id).expect("find tree");
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .expect("commit");
        }
        repo
    }

    #[tokio::test]
    async fn reports_a_new_untracked_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        std::fs::write(dir.path().join("b.txt"), "new file\n").expect("write file");

        let tool = GitStatusTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({}),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        let entries = result.output["entries"].as_array().expect("entries array");
        assert!(entries.iter().any(|e| e["path"] == "b.txt" && e["wt_new"] == true));
    }

    #[tokio::test]
    async fn reports_a_modified_tracked_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        std::fs::write(dir.path().join("a.txt"), "changed\n").expect("modify file");

        let tool = GitStatusTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({}),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        let entries = result.output["entries"].as_array().expect("entries array");
        assert!(entries
            .iter()
            .any(|e| e["path"] == "a.txt" && e["wt_modified"] == true));
    }

    #[tokio::test]
    async fn errors_gracefully_outside_a_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = GitStatusTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({}),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
    }
}

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

/// Same cap as `git.diff` — a single commit's diff can be arbitrarily large.
const MAX_DIFF_BYTES: usize = 50_000;

/// Input for the `git.show` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitShowInput {
    /// A commit-ish revision (SHA, branch name, `HEAD~2`, etc.).
    pub rev: String,
}

/// Shows a single commit's metadata and diff by ref/SHA. Read-only.
pub struct GitShowTool {
    repo_root: PathBuf,
}

impl GitShowTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

#[async_trait]
impl ToolExecutor for GitShowTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(GitShowInput);
        ToolDescriptor {
            id: ToolId::new("git.show"),
            name: "Git show".to_string(),
            description: "Show a single commit's metadata and diff by ref/SHA".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: GitShowInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let repo_root = self.repo_root.clone();
        let result = tokio::task::spawn_blocking(move || run_show(&repo_root, &input.rev))
            .await
            .map_err(|_| ToolError::Internal)?;

        match result {
            Ok(details) => Ok(ToolResult {
                call_id: "git.show".to_string(),
                output: details,
                is_error: false,
            }),
            Err(message) => Ok(ToolResult {
                call_id: "git.show".to_string(),
                output: json!({ "error": message }),
                is_error: true,
            }),
        }
    }
}

fn render_patch(diff: &git2::Diff) -> Result<String, String> {
    let mut buffer = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let content = std::str::from_utf8(line.content()).unwrap_or("");
        match line.origin() {
            '+' | '-' | ' ' => {
                buffer.push(line.origin());
                buffer.push_str(content);
            }
            _ => buffer.push_str(content),
        }
        true
    })
    .map_err(|e| e.to_string())?;

    if buffer.len() > MAX_DIFF_BYTES {
        buffer.truncate(MAX_DIFF_BYTES);
        buffer.push_str("\n... (diff truncated)");
    }
    Ok(buffer)
}

fn run_show(repo_root: &Path, rev: &str) -> Result<serde_json::Value, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;
    let object = repo.revparse_single(rev).map_err(|e| e.to_string())?;
    let commit = object.peel_to_commit().map_err(|e| e.to_string())?;
    let tree = commit.tree().map_err(|e| e.to_string())?;
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());

    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
        .map_err(|e| e.to_string())?;
    let patch = render_patch(&diff)?;

    Ok(json!({
        "sha": commit.id().to_string(),
        "summary": commit.summary().ok().flatten().unwrap_or("").to_string(),
        "author": commit.author().name().unwrap_or("").to_string(),
        "time": commit.time().seconds(),
        "diff": patch,
    }))
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
    async fn shows_head_commit_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());

        let tool = GitShowTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "rev": "HEAD" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        assert_eq!(result.output["summary"], "initial commit");
        let diff = result.output["diff"].as_str().expect("diff string");
        assert!(diff.contains("+hello"));
    }

    #[tokio::test]
    async fn errors_gracefully_on_unknown_rev() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());

        let tool = GitShowTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "rev": "not-a-real-rev" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should not hard-fail");
        assert!(result.is_error);
    }
}

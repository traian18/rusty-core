use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

/// Caps returned diff size so an agent asking for a diff on a huge file
/// can't blow the context window with one tool result.
const MAX_DIFF_BYTES: usize = 50_000;

/// Input for the `git.diff` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitDiffInput {
    /// Optional path filter, relative to the repo root.
    #[serde(default)]
    pub path: Option<String>,
    /// `false` (default): working tree vs index. `true`: index vs HEAD.
    #[serde(default)]
    pub staged: bool,
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
            description: "Show a diff for a path or the whole tree (working tree or staged)"
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
        let result = tokio::task::spawn_blocking(move || {
            run_diff(&repo_root, input.path.as_deref(), input.staged)
        })
        .await
        .map_err(|_| ToolError::Internal)?;

        match result {
            Ok(patch) => Ok(ToolResult {
                call_id: "git.diff".to_string(),
                output: json!({ "diff": patch }),
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

fn run_diff(repo_root: &Path, path: Option<&str>, staged: bool) -> Result<String, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;

    let mut options = git2::DiffOptions::new();
    if let Some(path) = path {
        options.pathspec(path);
    }

    let diff = if staged {
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

    render_patch(&diff)
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
    async fn shows_unstaged_working_tree_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").expect("modify file");

        let tool = GitDiffTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "staged": false }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        let diff = result.output["diff"].as_str().expect("diff string");
        assert!(diff.contains("+world"));
    }

    #[tokio::test]
    async fn shows_staged_diff_against_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo_with_commit(dir.path());
        std::fs::write(dir.path().join("a.txt"), "staged change\n").expect("modify file");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.txt")).expect("add path");
        index.write().expect("write index");

        let tool = GitDiffTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "staged": true }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        let diff = result.output["diff"].as_str().expect("diff string");
        assert!(diff.contains("staged change"));
    }
}

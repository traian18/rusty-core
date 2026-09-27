use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

use crate::patch::{apply_pathspecs, render, DiffFilters};

fn default_include_diff() -> bool {
    true
}

/// Input for the `git.show` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitShowInput {
    /// A commit-ish revision (SHA, branch name, `HEAD~2`, etc.).
    pub rev: String,
    /// `false`: return commit metadata and per-file change counts only.
    #[serde(default = "default_include_diff")]
    pub include_diff: bool,
    #[serde(flatten)]
    pub filters: DiffFilters,
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
            description: "Show a single commit's metadata, per-file change counts, and diff by ref/SHA. \
                          The diff is limited by file count and size; narrow with paths, hunk_contains, \
                          max_hunks_per_file, or set include_diff to false."
                .to_string(),
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
        let result = tokio::task::spawn_blocking(move || run_show(&repo_root, &input))
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

fn run_show(repo_root: &Path, input: &GitShowInput) -> Result<serde_json::Value, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;
    let object = repo
        .revparse_single(&input.rev)
        .map_err(|e| e.to_string())?;
    let commit = object.peel_to_commit().map_err(|e| e.to_string())?;
    let tree = commit.tree().map_err(|e| e.to_string())?;
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());

    let mut options = git2::DiffOptions::new();
    apply_pathspecs(&mut options, input.filters.paths.iter().map(String::as_str));
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
        .map_err(|e| e.to_string())?;

    let mut output = serde_json::Map::new();
    output.insert("sha".into(), json!(commit.id().to_string()));
    output.insert(
        "summary".into(),
        json!(commit.summary().ok().flatten().unwrap_or("")),
    );
    output.insert("author".into(), json!(commit.author().name().unwrap_or("")));
    output.insert("time".into(), json!(commit.time().seconds()));
    output.extend(render(&diff, &input.filters, input.include_diff)?);
    Ok(serde_json::Value::Object(output))
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

    #[tokio::test]
    async fn include_diff_false_returns_metadata_and_file_counts_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());

        let result = GitShowTool::new(dir.path().to_path_buf())
            .execute(
                ToolInput {
                    arguments: json!({ "rev": "HEAD", "include_diff": false }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        assert_eq!(result.output["summary"], "initial commit");
        assert_eq!(result.output["diff"], "");
        assert_eq!(result.output["files"][0]["path"], "a.txt");
        assert_eq!(result.output["files"][0]["status"], "added");
        assert_eq!(result.output["files"][0]["additions"], 1);
        assert_eq!(result.output["files"][0]["patch"], "summary");
    }

    #[tokio::test]
    async fn filters_a_commit_to_the_requested_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("keep.rs"), "fn keep() {}\n").expect("write");
        std::fs::write(dir.path().join("skip.txt"), "skip\n").expect("write");
        let mut index = repo.index().expect("index");
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .expect("add all");
        index.write().expect("write index");
        let tree = repo
            .find_tree(index.write_tree().expect("write tree"))
            .expect("tree");
        let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
        repo.commit(Some("HEAD"), &sig, &sig, "two files", &tree, &[])
            .expect("commit");

        let result = GitShowTool::new(dir.path().to_path_buf())
            .execute(
                ToolInput {
                    arguments: json!({ "rev": "HEAD", "paths": ["*.rs"] }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert_eq!(result.output["total_files"], 1);
        assert_eq!(result.output["files"][0]["path"], "keep.rs");
        let diff = result.output["diff"].as_str().unwrap();
        assert!(diff.contains("+fn keep() {}"));
        assert!(diff.contains("--- /dev/null"));
        assert!(!diff.contains("skip.txt"));
    }
}

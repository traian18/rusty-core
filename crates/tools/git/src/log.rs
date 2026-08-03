use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};

fn default_limit() -> u32 {
    20
}

/// Hard cap enforced regardless of what's requested, so a runaway request
/// can't return the entire project history.
const MAX_LOG_LIMIT: u32 = 200;

/// Upper bound on commits *examined* when a path filter is set — without
/// this, a path filter on a huge repo would walk the entire history to find
/// a handful of matches.
const MAX_COMMITS_WALKED: usize = 5_000;

/// Input for the `git.log` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GitLogInput {
    /// Optional path filter, relative to the repo root.
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum number of commits to return (default 20, hard cap 200).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

/// Lists recent commits, optionally filtered to those touching a path.
/// Read-only.
pub struct GitLogTool {
    repo_root: PathBuf,
}

impl GitLogTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

#[async_trait]
impl ToolExecutor for GitLogTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(GitLogInput);
        ToolDescriptor {
            id: ToolId::new("git.log"),
            name: "Git log".to_string(),
            description: "List recent commits, optionally filtered to those touching a path".to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: GitLogInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let repo_root = self.repo_root.clone();
        let result = tokio::task::spawn_blocking(move || {
            run_log(&repo_root, input.path.as_deref(), input.limit)
        })
        .await
        .map_err(|_| ToolError::Internal)?;

        match result {
            Ok(entries) => Ok(ToolResult {
                call_id: "git.log".to_string(),
                output: json!({ "entries": entries }),
                is_error: false,
            }),
            Err(message) => Ok(ToolResult {
                call_id: "git.log".to_string(),
                output: json!({ "error": message }),
                is_error: true,
            }),
        }
    }
}

fn commit_touches_path(repo: &git2::Repository, commit: &git2::Commit, path: &str) -> bool {
    let Ok(tree) = commit.tree() else { return false };
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    let mut options = git2::DiffOptions::new();
    options.pathspec(path);
    match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options)) {
        Ok(diff) => diff.deltas().len() > 0,
        Err(_) => false,
    }
}

fn run_log(repo_root: &Path, path: Option<&str>, limit: u32) -> Result<Vec<serde_json::Value>, String> {
    let repo = git2::Repository::discover(repo_root).map_err(|e| e.to_string())?;
    let mut revwalk = repo.revwalk().map_err(|e| e.to_string())?;
    revwalk.push_head().map_err(|e| e.to_string())?;

    let limit = (limit.clamp(1, MAX_LOG_LIMIT)) as usize;
    let mut entries = Vec::new();

    for (walked, oid) in revwalk.enumerate() {
        if entries.len() >= limit || walked >= MAX_COMMITS_WALKED {
            break;
        }
        let oid = oid.map_err(|e| e.to_string())?;
        let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;

        if let Some(path) = path {
            if !commit_touches_path(&repo, &commit, path) {
                continue;
            }
        }

        entries.push(json!({
            "sha": commit.id().to_string(),
            "summary": commit.summary().unwrap_or("").to_string(),
            "author": commit.author().name().unwrap_or("").to_string(),
            "time": commit.time().seconds(),
        }));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo_with_commits(dir: &Path, count: usize) -> git2::Repository {
        let repo = git2::Repository::init(dir).expect("init repo");
        let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
        let mut parents: Vec<git2::Oid> = Vec::new();
        for i in 0..count {
            std::fs::write(dir.join("a.txt"), format!("version {i}\n")).expect("write file");
            let mut index = repo.index().expect("index");
            index.add_path(Path::new("a.txt")).expect("add path");
            index.write().expect("write index");
            let tree_id = index.write_tree().expect("write tree");
            let tree = repo.find_tree(tree_id).expect("find tree");
            let parent_commits: Vec<git2::Commit> = parents
                .last()
                .map(|oid| vec![repo.find_commit(*oid).expect("find parent")])
                .unwrap_or_default();
            let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
            let oid = repo
                .commit(Some("HEAD"), &sig, &sig, &format!("commit {i}"), &tree, &parent_refs)
                .expect("commit");
            parents.push(oid);
        }
        repo
    }

    #[tokio::test]
    async fn lists_commits_most_recent_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commits(dir.path(), 3);

        let tool = GitLogTool::new(dir.path().to_path_buf());
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
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["summary"], "commit 2");
        assert_eq!(entries[2]["summary"], "commit 0");
    }

    #[tokio::test]
    async fn respects_the_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commits(dir.path(), 5);

        let tool = GitLogTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "limit": 2 }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        let entries = result.output["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn clamps_a_limit_above_the_hard_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo_with_commits(dir.path(), 2);

        let tool = GitLogTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                ToolInput {
                    arguments: json!({ "limit": 999_999 }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");

        assert!(!result.is_error);
        let entries = result.output["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 2); // repo only has 2 commits — cap didn't blow up
    }
}

use std::path::{Path, PathBuf};

use crate::workspace::{FileInfo, SearchResult, Workspace, WorkspaceError, WorkspaceMode};
use crate::FsWorkspace;

/// A workspace backed by a real git worktree checked out from the given
/// repository. Child edits and commits made inside the worktree are isolated
/// onto a dedicated branch and never appear on the parent repository's own
/// branch.
pub struct WorktreeWorkspace {
    inner: FsWorkspace,
    worktree_path: PathBuf,
    repo_root: PathBuf,
}

impl WorktreeWorkspace {
    /// Create a new git worktree checked out from `repo_root` on a branch
    /// derived from `branch_hint`. The worktree is placed at
    /// `<repo_root>/.harness-worktrees/<branch_hint>`.
    ///
    /// The synchronous `git2` calls are wrapped in
    /// [`tokio::task::spawn_blocking`] so they never block the async runtime.
    pub async fn create(repo_root: &Path, branch_hint: &str) -> Result<Self, WorkspaceError> {
        let repo_root = repo_root.to_path_buf();
        let worktree_path = repo_root.join(".harness-worktrees").join(branch_hint);
        let repo_root2 = repo_root.clone();
        let path2 = worktree_path.clone();
        // `spawn_blocking` requires a `'static` closure, so the borrow of
        // `branch_hint` must become an owned value before the move.
        let branch_hint = branch_hint.to_string();

        tokio::task::spawn_blocking(move || {
            let repo = git2::Repository::open(&repo_root2).map_err(WorkspaceError::from_git)?;
            // git2 does not create intermediate directories for the worktree
            // path; ensure the container directory exists first.
            if let Some(parent) = path2.parent() {
                std::fs::create_dir_all(parent).map_err(WorkspaceError::from)?;
            }
            repo.worktree(&branch_hint, &path2, None)
                .map_err(WorkspaceError::from_git)?;
            Ok::<_, WorkspaceError>(())
        })
        .await
        .map_err(|e| WorkspaceError::ToolFailed(e.to_string()))??;

        Ok(Self {
            inner: FsWorkspace::new(worktree_path.clone()),
            worktree_path,
            repo_root,
        })
    }

    /// The path of the underlying git worktree on disk.
    pub fn worktree_path(&self) -> &Path {
        &self.worktree_path
    }

    /// The path of the parent repository the worktree was created from.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

#[async_trait::async_trait]
impl Workspace for WorktreeWorkspace {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Isolated
    }

    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError> {
        self.inner.read(relative_path).await
    }

    async fn write(&self, relative_path: &str, content: &str) -> Result<(), WorkspaceError> {
        self.inner.write(relative_path, content).await
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        self.inner.search(query).await
    }

    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        self.inner.list_files(max_depth).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a throwaway git repository at `dir` containing a single committed
    /// file on `master`, so the worktree has a branch to check out.
    fn init_repo(dir: &Path) {
        let repo = git2::Repository::init(dir).unwrap();

        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("README.md"), "# initial\n").unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();

        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        // A freshly initialized repo may have an unborn HEAD. Commit to HEAD
        // first (creating the initial commit), then make `master` point at it.
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());

        let commit_id = match parent {
            Some(parent) => repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "initial commit",
                    &tree,
                    &[&parent],
                )
                .unwrap(),
            None => repo
                .commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap(),
        };
        let commit = repo.find_commit(commit_id).unwrap();

        // Ensure `master` points at the new commit. If `master` already exists
        // (e.g. it is the repo's default branch and HEAD already moved it),
        // leave it alone — git2 refuses to force-create a branch that is the
        // current HEAD.
        if repo.find_branch("master", git2::BranchType::Local).is_err() {
            repo.branch("master", &commit, true).unwrap();
        }
        repo.set_head("refs/heads/master").unwrap();
    }

    /// Whether any commit reachable from `repo_root`'s `master` mentions the
    /// given message substring.
    fn branch_has_commit_message(repo_root: &Path, needle: &str) -> bool {
        let repo = git2::Repository::open(repo_root).unwrap();
        let master = repo.find_branch("master", git2::BranchType::Local).unwrap();
        let commit = master.get().peel_to_commit().unwrap();
        let mut revwalk = repo.revwalk().unwrap();
        revwalk.push(commit.id()).unwrap();
        for oid in revwalk {
            let oid = oid.unwrap();
            let c = repo.find_commit(oid).unwrap();
            if c.message().map(|m| m.contains(needle)).unwrap_or(false) {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn worktree_isolates_child_commits() {
        let repo_dir = tempdir().unwrap();
        init_repo(repo_dir.path());

        // Spawn the worktree workspace.
        let ws = WorktreeWorkspace::create(repo_dir.path(), "task-42")
            .await
            .unwrap();

        // The worktree must be a real directory on disk, inside the parent repo.
        let wt_root = ws.worktree_path().to_path_buf();
        assert!(
            wt_root.is_dir(),
            "worktree must be a real directory on disk"
        );

        // Child writes a file through the workspace...
        ws.write("child.txt", "child change\n").await.unwrap();

        // ...then commits it inside the worktree on its own branch.
        let wt_path = ws.worktree_path().to_path_buf();
        let repo_root_path = ws.repo_root().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let repo = git2::Repository::open(&wt_path).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("child.txt")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let sig = git2::Signature::now("Test", "test@example.com").unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "child worktree commit",
                &repo.find_tree(tree_id).unwrap(),
                &[&head],
            )
            .unwrap();
        })
        .await
        .unwrap();

        // The child's commit must NOT appear on the parent repo's branch.
        assert!(
            !branch_has_commit_message(&repo_root_path, "child worktree commit"),
            "child commit must not appear on the parent branch"
        );

        // The worktree directory still exists on disk.
        assert!(wt_root.is_dir());
    }
}

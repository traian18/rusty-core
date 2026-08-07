use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;

/// Controls how workspace instances observe each other's writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceMode {
    /// Multiple handles on the same root see each other's writes (default).
    #[default]
    Shared,
    /// Writes from one handle are invisible to others (stub — returns errors).
    Isolated,
}

#[derive(thiserror::Error, Debug)]
pub enum WorkspaceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path outside workspace root: {path}")]
    PathTraversal { path: PathBuf },

    #[error("Isolated workspace does not support this operation")]
    Isolated,

    #[error("Tool execution failed: {0}")]
    ToolFailed(String),

    #[error("Git error: {0}")]
    Git(String),
}

impl WorkspaceError {
    /// Convert a `git2::Error` into a [`WorkspaceError::Git`].
    pub fn from_git(err: git2::Error) -> Self {
        WorkspaceError::Git(err.to_string())
    }
}

/// Represents a single search match.
#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub file_path: PathBuf,
    pub line_number: usize,
    pub line_content: String,
}

/// Result of a workspace search operation.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub total_count: usize,
    /// M3: `true` when traversal was stopped early (files-scanned or
    /// matches-collected cap reached) rather than having examined the whole
    /// workspace tree — callers must not treat `matches` as exhaustive.
    #[serde(default)]
    pub truncated: bool,
}

/// Metadata about a file in the workspace.
#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub is_directory: bool,
}

/// Progress event emitted during long-running tool operations.
#[derive(Debug, Clone, Serialize)]
pub struct ToolProgress {
    pub tool_call_id: String,
    pub phase: ProgressPhase,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub enum ProgressPhase {
    Started,
    Streaming,
    Completed,
    Failed,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub success: bool,
    pub content: String,
}

/// Abstract workspace access for tools.
#[async_trait::async_trait]
pub trait Workspace: Send + Sync {
    fn root(&self) -> &Path;
    fn mode(&self) -> WorkspaceMode;

    /// Read a file relative to the workspace root.
    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError>;

    /// Write (replace) a file relative to the workspace root.
    async fn write(&self, relative_path: &str, content: &str) -> Result<(), WorkspaceError>;

    /// Search for `query` (case-insensitive substring) in all UTF-8 text files.
    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError>;

    /// List files recursively (up to `max_depth`, 0 = unlimited).
    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError>;
}

/// A workspace that exposes only read-only access to an underlying workspace.
/// Permits reads, searches, and listings; rejects all writes with
/// [`WorkspaceError::Isolated`].
pub struct IsolatedWorkspace {
    inner: Arc<dyn Workspace>,
}

/// Read-only view over a workspace. Writes are rejected with
/// [`WorkspaceError::Isolated`]; all other operations delegate to the inner
/// workspace unchanged.
pub type ReadOnlyWorkspace = IsolatedWorkspace;

impl IsolatedWorkspace {
    pub fn new(inner: Arc<dyn Workspace>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Workspace for IsolatedWorkspace {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Isolated
    }

    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError> {
        self.inner.read(relative_path).await
    }

    async fn write(&self, _relative_path: &str, _content: &str) -> Result<(), WorkspaceError> {
        Err(WorkspaceError::Isolated)
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        self.inner.search(query).await
    }

    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        self.inner.list_files(max_depth).await
    }
}

/// A workspace backed by a recursive copy of a source tree placed in a fresh
/// temp directory. Reads and writes hit the snapshot copy, leaving the source
/// tree untouched — child edits are invisible to any other handle on the
/// original root.
pub struct SnapshotWorkspace {
    inner: crate::FsWorkspace,
    _origin: Arc<dyn Workspace>,
}

impl SnapshotWorkspace {
    /// Create a new snapshot of `source_root` rooted at
    /// `<temp_dir>/harness-snapshot-<uuid>`.
    pub async fn create(source_root: &Path) -> Result<Self, WorkspaceError> {
        let temp_root = std::env::temp_dir().join(format!(
            "harness-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        copy_dir_recursive(source_root.to_path_buf(), temp_root.clone()).await?;
        Ok(Self {
            inner: crate::FsWorkspace::new(temp_root),
            _origin: Arc::new(crate::FsWorkspace::new(source_root.to_path_buf())),
        })
    }
}

#[async_trait::async_trait]
impl Workspace for SnapshotWorkspace {
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

/// Recursively copy the contents of `src` into `dst` (a plain, non-COW copy).
/// The destination directory is created if missing.
///
/// The recursion is boxed (`Pin<Box<dyn Future>>`) because recursive `async
/// fn`s require indirection to avoid an infinitely sized future; the helper
/// takes owned paths so the boxed future needs no borrows and stays `Send`.
fn copy_dir_recursive(
    src: PathBuf,
    dst: PathBuf,
) -> Pin<Box<dyn Future<Output = Result<(), WorkspaceError>> + Send>> {
    Box::pin(async move {
        tokio::fs::create_dir_all(&dst).await?;
        let mut entries = tokio::fs::read_dir(&src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_recursive(src_path, dst_path).await?;
            } else {
                tokio::fs::copy(&src_path, &dst_path).await?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod isolated_tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn isolated_workspace_allows_reads() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("data.txt"), "secret")
            .await
            .unwrap();

        let shared = crate::FsWorkspace::new(dir.path().to_path_buf());
        let isolated = IsolatedWorkspace::new(Arc::new(shared));

        let contents = isolated.read("data.txt").await.unwrap();
        assert_eq!(contents, "secret");
    }

    #[tokio::test]
    async fn isolated_workspace_rejects_writes() {
        let dir = tempdir().unwrap();
        let shared = crate::FsWorkspace::new(dir.path().to_path_buf());
        let isolated = IsolatedWorkspace::new(Arc::new(shared));

        let result = isolated.write("data.txt", "new content").await;
        assert!(matches!(result, Err(WorkspaceError::Isolated)));
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use tempfile::tempdir;

    /// The hardened `ReadOnlyWorkspace` (alias for `IsolatedWorkspace`) must
    /// reject writes with [`WorkspaceError::Isolated`].
    #[tokio::test]
    async fn readonly_workspace_rejects_writes() {
        let dir = tempdir().unwrap();
        let shared = crate::FsWorkspace::new(dir.path().to_path_buf());
        let read_only = ReadOnlyWorkspace::new(Arc::new(shared));

        let result = read_only.write("data.txt", "new content").await;
        assert!(matches!(result, Err(WorkspaceError::Isolated)));
    }

    /// A snapshot copy must isolate edits: writes through the child snapshot
    /// never reach the parent's original root.
    #[tokio::test]
    async fn snapshot_workspace_isolates_writes() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("data.txt"), "original")
            .await
            .unwrap();

        let snapshot = SnapshotWorkspace::create(dir.path()).await.unwrap();
        let parent = crate::FsWorkspace::new(dir.path().to_path_buf());

        // Child writes to the snapshot root.
        snapshot.write("data.txt", "child edit").await.unwrap();

        // The snapshot reflects the child's edit...
        assert_eq!(snapshot.read("data.txt").await.unwrap(), "child edit");

        // ...while the parent's original root is unchanged.
        assert_eq!(parent.read("data.txt").await.unwrap(), "original");
    }
}

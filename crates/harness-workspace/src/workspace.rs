use std::path::{Path, PathBuf};

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

/// A workspace stub that pretends writes are invisible to other handles.
/// Permits reads; rejects writes with [`WorkspaceError::Isolated`].
///
/// In a future phase this will be backed by a temporary copy-on-write
/// tree. For now it exists to satisfy the capability surface.
pub struct IsolatedWorkspace {
    inner: std::sync::Arc<dyn Workspace>,
}

impl IsolatedWorkspace {
    pub fn new(inner: std::sync::Arc<dyn Workspace>) -> Self {
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
        let isolated = IsolatedWorkspace::new(std::sync::Arc::new(shared));

        let contents = isolated.read("data.txt").await.unwrap();
        assert_eq!(contents, "secret");
    }

    #[tokio::test]
    async fn isolated_workspace_rejects_writes() {
        let dir = tempdir().unwrap();
        let shared = crate::FsWorkspace::new(dir.path().to_path_buf());
        let isolated = IsolatedWorkspace::new(std::sync::Arc::new(shared));

        let result = isolated.write("data.txt", "new content").await;
        assert!(matches!(result, Err(WorkspaceError::Isolated)));
    }
}

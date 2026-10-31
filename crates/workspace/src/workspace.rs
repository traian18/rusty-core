//! The [`Workspace`] contract: how tools read, write, search and list files in a project,
//! independent of where the files really live.

use std::path::Path;

use crate::{
    error::WorkspaceError,
    listing::{FileInfo, SearchResult},
};

/// Controls how workspace instances observe each other's writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceMode {
    /// Multiple handles on the same root see each other's writes (default).
    #[default]
    Shared,
    /// Writes from one handle are invisible to others (stub — returns errors).
    Isolated,
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

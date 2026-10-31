//! A workspace view that can be read but never changed.

use std::{path::Path, sync::Arc};

use crate::{
    error::WorkspaceError,
    listing::{FileInfo, SearchResult},
    workspace::{Workspace, WorkspaceMode},
};

/// A view of another workspace that allows reads, searches and listings and refuses every write
/// with [`WorkspaceError::Isolated`]. Child agents that must not change files get this.
pub struct ReadOnlyWorkspace {
    inner: Arc<dyn Workspace>,
}

impl ReadOnlyWorkspace {
    pub fn new(inner: Arc<dyn Workspace>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Workspace for ReadOnlyWorkspace {
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
mod tests;

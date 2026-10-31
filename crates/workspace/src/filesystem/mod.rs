//! The workspace backed by a real directory on disk.
//!
//! Every operation is a short list of steps; the steps live next to it: `path_guard` keeps paths
//! inside the root, `read` and `write` move bytes, `search` and `list` walk the tree.

mod list;
mod path_guard;
mod read;
mod search;
mod write;

use std::path::{Path, PathBuf};

use tracing::info;

use crate::{
    error::WorkspaceError,
    listing::{FileInfo, SearchResult},
    workspace::{Workspace, WorkspaceMode},
};

/// A workspace over the directory `root`. Paths are relative to it, and any that would leave it,
/// or pass through a symlink, are refused with [`WorkspaceError::PathTraversal`].
pub struct FsWorkspace {
    root: PathBuf,
    mode: WorkspaceMode,
}

impl FsWorkspace {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mode: WorkspaceMode::Shared,
        }
    }

    pub fn with_mode(mut self, mode: WorkspaceMode) -> Self {
        self.mode = mode;
        self
    }
}

#[async_trait::async_trait]
impl Workspace for FsWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn mode(&self) -> WorkspaceMode {
        self.mode
    }

    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError> {
        let path = path_guard::resolve_without_symlinks(&self.root, relative_path).await?;
        read::read_capped(&path).await
    }

    async fn write(&self, relative_path: &str, content: &str) -> Result<(), WorkspaceError> {
        if self.mode == WorkspaceMode::Isolated {
            return Err(WorkspaceError::Isolated);
        }
        let target = write::prepare_target(&self.root, relative_path).await?;
        write::write_atomically(&self.root, &target, content).await?;
        info!(path = %relative_path, "wrote file to workspace");
        Ok(())
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        search::search_tree(&self.root, query).await
    }

    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        list::list_tree(&self.root, max_depth).await
    }
}

#[cfg(test)]
mod tests;

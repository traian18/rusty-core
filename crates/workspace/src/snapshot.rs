//! A workspace that works on a private copy of a source tree.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use crate::{
    error::WorkspaceError,
    filesystem::FsWorkspace,
    listing::{FileInfo, SearchResult},
    workspace::{Workspace, WorkspaceMode},
};

/// A workspace backed by a recursive copy of a source tree placed in a fresh
/// temp directory. Reads and writes hit the snapshot copy, leaving the source
/// tree untouched — child edits are invisible to any other handle on the
/// original root.
pub struct SnapshotWorkspace {
    inner: FsWorkspace,
    _origin: Arc<dyn Workspace>,
}

impl SnapshotWorkspace {
    /// Create a new snapshot of `source_root` rooted at
    /// `<temp_dir>/harness-snapshot-<uuid>`.
    pub async fn create(source_root: &Path) -> Result<Self, WorkspaceError> {
        let temp_root =
            std::env::temp_dir().join(format!("harness-snapshot-{}", uuid::Uuid::new_v4()));
        copy_dir_recursive(source_root.to_path_buf(), temp_root.clone()).await?;
        Ok(Self {
            inner: FsWorkspace::new(temp_root),
            _origin: Arc::new(FsWorkspace::new(source_root.to_path_buf())),
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
mod tests;

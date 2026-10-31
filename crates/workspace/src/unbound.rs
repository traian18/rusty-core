//! The workspace of a session that was never given one.

use std::path::{Path, PathBuf};

use crate::{
    error::WorkspaceError,
    listing::{FileInfo, SearchResult},
    workspace::{Workspace, WorkspaceMode},
};

/// The placeholder bound to a session that never had a real workspace.
///
/// Every operation fails with [`WorkspaceError::Unbound`]. This exists so an
/// unbound session is *inert* rather than *quietly wrong*: a session with no
/// workspace-touching tools never calls into it and behaves exactly as
/// before, while one that does gets an actionable error at the first tool
/// call instead of reads that report "not found" and writes that vanish.
#[derive(Debug, Default)]
pub struct UnboundWorkspace {
    root: PathBuf,
}

impl UnboundWorkspace {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Workspace for UnboundWorkspace {
    fn root(&self) -> &Path {
        // Empty rather than a plausible-looking path — nothing should be
        // resolving against this, and "" makes that obvious in diagnostics.
        &self.root
    }

    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Isolated
    }

    async fn read(&self, _relative_path: &str) -> Result<String, WorkspaceError> {
        Err(WorkspaceError::Unbound)
    }

    async fn write(&self, _relative_path: &str, _content: &str) -> Result<(), WorkspaceError> {
        Err(WorkspaceError::Unbound)
    }

    async fn search(&self, _query: &str) -> Result<SearchResult, WorkspaceError> {
        Err(WorkspaceError::Unbound)
    }

    async fn list_files(&self, _max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        Err(WorkspaceError::Unbound)
    }
}

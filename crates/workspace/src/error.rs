//! Why a workspace operation failed.

use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum WorkspaceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path outside workspace root: {path}")]
    PathTraversal { path: PathBuf },

    #[error("Isolated workspace does not support this operation")]
    Isolated,

    /// No workspace was bound to the session, but a tool tried to use one.
    ///
    /// Sessions without workspace-touching tools never hit this; it exists so
    /// that forgetting to bind a workspace fails loudly at first use instead
    /// of silently succeeding against throwaway in-memory storage.
    #[error(
        "no workspace is bound to this session; bind one with \
         SessionBuilder::workspace or SessionBuilder::toolset before using \
         workspace-backed tools"
    )]
    Unbound,

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

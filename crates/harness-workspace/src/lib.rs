#![warn(clippy::all)]

//! Abstract workspace operations and filesystem-backed workspace implementation.

pub mod filesystem;
pub mod workspace;
pub mod worktree;

pub use filesystem::FsWorkspace;
pub use workspace::{
    FileInfo, ProgressPhase, ReadOnlyWorkspace, SearchMatch, SearchResult, SnapshotWorkspace,
    ToolProgress, ToolResult, Workspace, WorkspaceError, WorkspaceMode,
};
pub use worktree::WorktreeWorkspace;

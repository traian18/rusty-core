#![warn(clippy::all)]

//! Abstract workspace operations and filesystem-backed workspace implementation.

pub mod filesystem;
pub mod worktree;
pub mod workspace;

pub use filesystem::FsWorkspace;
pub use worktree::WorktreeWorkspace;
pub use workspace::{
    FileInfo, ProgressPhase, ReadOnlyWorkspace, SearchMatch, SearchResult, SnapshotWorkspace,
    ToolProgress, ToolResult, Workspace, WorkspaceError, WorkspaceMode,
};

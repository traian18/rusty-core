#![warn(clippy::all)]

//! Abstract workspace operations and filesystem-backed workspace implementation.

pub mod filesystem;
pub mod workspace;

pub use filesystem::FsWorkspace;
pub use workspace::{
    FileInfo, ProgressPhase, SearchMatch, SearchResult, ToolProgress, ToolResult, Workspace,
    WorkspaceError, WorkspaceMode,
};

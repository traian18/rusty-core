#![warn(clippy::all)]

//! The [`Workspace`] abstraction: how tools see files in a project, whether they live on disk, in
//! a private copy, in a git worktree, or nowhere yet.

pub mod cleanup;
mod error;
mod filesystem;
mod listing;
pub mod nested_repositories;
mod read_only;
mod snapshot;
mod tool_report;
mod unbound;
mod workspace;
mod worktree;

pub use error::WorkspaceError;
pub use filesystem::FsWorkspace;
pub use listing::{FileInfo, SearchMatch, SearchResult};
pub use read_only::ReadOnlyWorkspace;
pub use snapshot::SnapshotWorkspace;
pub use tool_report::{ProgressPhase, ToolProgress, ToolResult};
pub use unbound::UnboundWorkspace;
pub use workspace::{Workspace, WorkspaceMode};
pub use worktree::WorktreeWorkspace;

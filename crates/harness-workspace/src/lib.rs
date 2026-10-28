#![warn(clippy::all)]

//! Abstract workspace operations and filesystem-backed workspace implementation.

pub mod dependency_groups;
pub mod filesystem;
pub mod folder_tree;
pub mod framework_relations;
pub mod import_graph;
pub mod nested_repositories;
pub mod repository_intelligence;
pub mod workspace;
pub mod worktree;

pub use filesystem::FsWorkspace;
pub use workspace::{
    FileInfo, ProgressPhase, ReadOnlyWorkspace, SearchMatch, SearchResult, SnapshotWorkspace,
    ToolProgress, ToolResult, UnboundWorkspace, Workspace, WorkspaceError, WorkspaceMode,
};
pub use worktree::WorktreeWorkspace;

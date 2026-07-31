#![warn(clippy::all)]

//! High-level public harness and session API plus runtime composition.

pub mod builder;
pub mod harness;
pub mod session_builder;

pub use builder::HarnessBuilder;
pub use harness::Harness;
pub use session_builder::{HarnessError, SessionBuilder, SessionHandle};

// Re-export workspace types for convenience.
pub use harness_workspace::{
    FileInfo, ProgressPhase, SearchMatch, SearchResult, ToolProgress, ToolResult, Workspace,
    WorkspaceError, WorkspaceMode,
};

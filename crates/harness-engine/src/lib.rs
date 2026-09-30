#![warn(clippy::all)]

//! High-level public harness and session API plus runtime composition.

pub mod builder;
pub mod harness;
pub mod orchestration;
pub mod profiles;
pub mod providers;
pub mod session_builder;
pub mod validation;

pub use builder::HarnessBuilder;
pub use harness::Harness;
pub use harness_core::behavior::{BehaviorProfile, ProfileRef};
pub use harness_core::orchestration::{OrchestrationRunState, OrchestrationStatus};
pub use orchestration::{OrchestrationConfig, OrchestrationRequest, DEFAULT_ORCHESTRATION_ID};
pub use profiles::{import_hooks, CommandTrust, HooksImport, ProfilesConfig};
pub use providers::*;
pub use session_builder::{
    ContextInspection, HarnessError, McpServerConfig, McpTransportConfig, SessionBuilder,
    SessionHandle, SkillsConfig,
};

// Re-export workspace types for convenience.
pub use harness_workspace::{
    FileInfo, FsWorkspace, ProgressPhase, SearchMatch, SearchResult, ToolProgress, ToolResult,
    Workspace, WorkspaceError, WorkspaceMode,
};

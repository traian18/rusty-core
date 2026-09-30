#![warn(clippy::all)]

//! Canonical tool traits, shared types, and registry implementations.
//!
//! Concrete executors are provided by dedicated crates such as
//! `harness-tool-filesystem` and `harness-tool-shell`.

pub mod call_context;
pub mod executor;
pub mod registry;

pub use call_context::{
    current_tool_call_id, current_tool_session_id, with_tool_call, with_tool_call_id,
};
pub use executor::{
    CancellationToken, ExecutionFailure, ExecutionResult, FailureKind, ProgressPhase,
    ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolProgress, ToolResult,
    ToolUsage, UnknownTool,
};
pub use registry::{RegistrationError, SimpleToolRegistry, ToolRegistry};

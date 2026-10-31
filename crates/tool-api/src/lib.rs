#![warn(clippy::all)]

//! Canonical tool traits, shared types, and registry implementations.
//!
//! Concrete executors are provided by dedicated crates such as
//! `rusty-tool-filesystem` and `rusty-tool-shell`.

mod call_context;
mod executor;
mod progress;
pub mod registry;
mod tool_call;
mod tool_id;

pub use call_context::{
    current_tool_call_id, current_tool_session_id, with_tool_call, with_tool_call_id,
};
pub use executor::{CancellationToken, ToolExecutor, UnknownTool};
pub use progress::{
    ExecutionFailure, ExecutionResult, FailureKind, ProgressPhase, ToolProgress, ToolUsage,
};
pub use registry::{RegistrationError, SimpleToolRegistry, ToolRegistry};
pub use tool_call::{ToolDescriptor, ToolError, ToolInput, ToolResult};
pub use tool_id::ToolId;

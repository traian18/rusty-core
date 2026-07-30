#![warn(clippy::all)]

//! Canonical tool traits, shared types, and registry implementations.
//!
//! Concrete executors are provided by dedicated crates such as
//! `harness-tool-filesystem` and `harness-tool-shell`.

pub mod executor;
pub mod registry;

pub use executor::{
    CancellationToken, ExecutionFailure, ExecutionResult, FailureKind, ProgressPhase,
    ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolProgress, ToolResult,
    ToolUsage, UnknownTool,
};
pub use registry::{RegistrationError, SimpleToolRegistry, ToolRegistry};

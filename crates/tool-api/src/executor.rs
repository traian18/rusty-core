//! The contract every concrete tool implements.
//!
//! Concrete tools live in their own crates (`rusty-tool-filesystem`, `rusty-tool-shell`, ...), so a
//! placeholder executor can never be registered in production by mistake.

use std::time::Duration;

use async_trait::async_trait;

use crate::tool_call::{ToolDescriptor, ToolError, ToolInput, ToolResult};

/// Re-exported so callers need no direct dependency on `tokio-util`.
pub use tokio_util::sync::CancellationToken;

/// The contract implemented by every concrete tool.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;

    /// How long the runtime lets one call run before cancelling it as timed
    /// out. `default` is the scheduler's limit for ordinary tools; a tool that
    /// legitimately runs longer (a build, a test suite) or waits on a person
    /// returns a longer limit, or `None` for no limit (it is still cancelled
    /// with its run).
    fn call_timeout(&self, _arguments: &serde_json::Value, default: Duration) -> Option<Duration> {
        Some(default)
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}

/// Fallback executor used for an unrecognized configured descriptor.
pub struct UnknownTool {
    pub descriptor: ToolDescriptor,
}

#[async_trait]
impl ToolExecutor for UnknownTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn execute(
        &self,
        _input: ToolInput,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::ExecutionFailed)
    }
}

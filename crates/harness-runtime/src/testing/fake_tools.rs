//! Scripted [`ToolExecutor`] and [`ToolRegistry`] test doubles.

use std::sync::Arc;
use std::collections::HashMap;

use async_trait::async_trait;

use crate::traits::{ToolDescriptor, ToolExecutor, ToolInput};
use crate::traits::ToolRegistry;
use harness_tools::{ToolError, ToolResult};

/// A test double for [`ToolExecutor`] that always returns a scripted result.
pub struct FakeToolExecutor {
    descriptor: ToolDescriptor,
    /// If `true`, `execute` blocks (awaiting cancellation) instead of
    /// returning immediately, simulating an in-flight tool call. Used to
    /// exercise mid-flight tool cancellation deterministically.
    block_until_cancelled: bool,
}

impl FakeToolExecutor {
    /// Create a new fake executor with the given descriptor.
    pub fn new(descriptor: ToolDescriptor) -> Self {
        Self {
            descriptor,
            block_until_cancelled: false,
        }
    }

    /// Builder method: block until the cancellation token fires instead of
    /// returning a scripted result immediately.
    ///
    /// Used to simulate a tool call that is still in flight so that
    /// mid-flight cancellation can be exercised deterministically.
    pub fn blocking_until_cancelled(mut self) -> Self {
        self.block_until_cancelled = true;
        self
    }
}

#[async_trait]
impl ToolExecutor for FakeToolExecutor {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn execute(
        &self,
        _input: ToolInput,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        if self.block_until_cancelled {
            cancel.cancelled().await;
            return Err(ToolError::ExecutionFailed);
        }

        Ok(ToolResult {
            call_id: "fake-call-id".to_string(),
            output: serde_json::json!({"message": "fake result"}),
            is_error: false,
        })
    }
}

/// A test double for [`ToolRegistry`] that returns a fixed set of executors.
pub struct FakeToolRegistry {
    executors: HashMap<String, Arc<dyn ToolExecutor>>,
}

impl FakeToolRegistry {
    /// Create an empty fake registry.
    pub fn new() -> Self {
        Self {
            executors: HashMap::new(),
        }
    }

    /// Add an executor to the registry.
    pub fn add_executor(&mut self, executor: Arc<dyn ToolExecutor>) {
        let descriptor = executor.descriptor();
        self.executors
            .insert(descriptor.id.to_string(), executor);
    }
}

impl Default for FakeToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolRegistry for FakeToolRegistry {
    fn register(&self, _executor: Arc<dyn ToolExecutor>) -> Result<(), crate::traits::RegistrationError> {
        Ok(())
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.executors.get(tool_id).cloned()
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.executors
            .values()
            .map(|executor| executor.descriptor())
            .collect()
    }
}

//! Scripted [`ToolExecutor`] and [`ToolRegistry`] test doubles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use harness_protocol::tools::{ToolCall, ToolDescriptor, ToolError, ToolResult};

use crate::traits::{ToolExecutor, ToolRegistry};

/// A scripted tool executor that replays a queue of pre-recorded results.
///
/// Each call to [`ToolExecutor::execute`] pops the next scripted result off
/// the front of the queue. Cancellation is checked before consulting the
/// script.
#[derive(Debug, Default)]
pub struct FakeToolExecutor {
    scripted_results: Mutex<Vec<Result<ToolResult, ToolError>>>,
}

impl FakeToolExecutor {
    /// Creates a new fake tool executor with the given scripted results,
    /// consumed in order (front to back).
    pub fn new(scripted_results: Vec<Result<ToolResult, ToolError>>) -> Self {
        Self {
            scripted_results: Mutex::new(scripted_results),
        }
    }
}

#[async_trait]
impl ToolExecutor for FakeToolExecutor {
    async fn execute(
        &self,
        _call: ToolCall,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        let mut scripted_results = self
            .scripted_results
            .lock()
            .expect("scripted_results mutex poisoned");

        if scripted_results.is_empty() {
            return Err(ToolError::Internal);
        }

        scripted_results.remove(0)
    }
}

/// A scripted tool registry backed by a fixed map of [`FakeToolExecutor`]s
/// and a fixed list of [`ToolDescriptor`]s.
#[derive(Debug, Default)]
pub struct FakeToolRegistry {
    executors: HashMap<String, Arc<FakeToolExecutor>>,
    descriptors: Vec<ToolDescriptor>,
}

impl FakeToolRegistry {
    /// Creates a new fake tool registry with the given executors and
    /// descriptors.
    pub fn new(
        executors: HashMap<String, Arc<FakeToolExecutor>>,
        descriptors: Vec<ToolDescriptor>,
    ) -> Self {
        Self {
            executors,
            descriptors,
        }
    }
}

impl ToolRegistry for FakeToolRegistry {
    fn lookup(&self, name: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.executors
            .get(name)
            .map(|executor| Arc::clone(executor) as Arc<dyn ToolExecutor>)
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.descriptors.clone()
    }
}

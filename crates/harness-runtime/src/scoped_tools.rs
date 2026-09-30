//! Tool and backend views that enforce a workflow node's tool allowlist.
//!
//! A scope is applied twice: descriptors outside the allowlist are removed
//! from backend requests, and executor lookup denies them at runtime. This
//! prevents a model from discovering a tool it cannot execute and prevents a
//! forged tool call from bypassing discovery-time filtering.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_tools::{RegistrationError, ToolDescriptor, ToolExecutor, ToolRegistry};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::traits::ExecutionBackend;

/// Read-only allowlisted view over an existing tool registry.
///
/// Registration is intentionally rejected: an attempt-specific scope must not
/// mutate the parent session's registry. Build and register tools on the parent
/// registry before creating this view.
pub struct ScopedToolRegistry {
    inner: Arc<dyn ToolRegistry>,
    allowed: HashSet<String>,
}

impl ScopedToolRegistry {
    pub fn new(
        inner: Arc<dyn ToolRegistry>,
        allowed_tools: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            inner,
            allowed: allowed_tools.into_iter().collect(),
        }
    }

    pub fn allows(&self, tool_id: &str) -> bool {
        self.allowed.contains(tool_id)
    }

    pub fn allowed_tools(&self) -> &HashSet<String> {
        &self.allowed
    }
}

#[async_trait]
impl ToolRegistry for ScopedToolRegistry {
    fn register(&self, executor: Arc<dyn ToolExecutor>) -> Result<(), RegistrationError> {
        // The trait has no read-only error variant. Delegating would mutate the
        // parent registry and violate attempt isolation, so report a duplicate
        // for the submitted ID. Callers must register before scoping.
        Err(RegistrationError::DuplicateToolId(executor.descriptor().id))
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.allows(tool_id)
            .then(|| self.inner.get_executor(tool_id))
            .flatten()
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.inner
            .descriptors()
            .into_iter()
            .filter(|descriptor| self.allows(descriptor.id.as_str()))
            .collect()
    }
}

/// Backend view that strips tools outside an attempt's allowlist from every
/// request immediately before provider execution.
pub struct ScopedExecutionBackend {
    inner: Arc<dyn ExecutionBackend>,
    allowed: HashSet<String>,
}

impl ScopedExecutionBackend {
    pub fn new(
        inner: Arc<dyn ExecutionBackend>,
        allowed_tools: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            inner,
            allowed: allowed_tools.into_iter().collect(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for ScopedExecutionBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn execute(
        &self,
        mut request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        request
            .tools
            .retain(|descriptor| self.allowed.contains(&descriptor.name));
        self.inner.execute(request, sink, cancel).await
    }
}

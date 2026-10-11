//! [`ContextAssemblingBackend`]: wraps an `ExecutionBackend`, running every
//! request through a [`ContextProvider`] first.
//!
//! Mirrors `ToolAdvertisingBackend` in
//! `crates/harness-engine/src/session_builder.rs`, which wraps a backend to
//! rewrite `request.tools` before delegating — this is the identical
//! decorator shape applied to `system_prompt`/`messages` instead, and is why
//! context assembly needs zero changes to `harness-core` or `agent_runner.rs`:
//! it's purely a backend-wrapping concern applied at
//! `SessionBuilder::start()` time (see `harness-engine`'s
//! `SessionBuilder::context_provider`).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_runtime::traits::{ExecutionBackend, Workspace};

use crate::provider::ContextProvider;

pub struct ContextAssemblingBackend {
    inner: Arc<dyn ExecutionBackend>,
    provider: Arc<dyn ContextProvider>,
    workspace: Arc<dyn Workspace>,
}

impl ContextAssemblingBackend {
    pub fn new(
        inner: Arc<dyn ExecutionBackend>,
        provider: Arc<dyn ContextProvider>,
        workspace: Arc<dyn Workspace>,
    ) -> Self {
        Self {
            inner,
            provider,
            workspace,
        }
    }
}

#[async_trait]
impl ExecutionBackend for ContextAssemblingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let mut request = self
            .provider
            .assemble(request, self.workspace.as_ref())
            .await;
        request.messages = harness_protocol::messages::repair_tool_history(request.messages);
        self.inner.execute(request, sink, cancel).await
    }
}

#[cfg(test)]
mod tests;

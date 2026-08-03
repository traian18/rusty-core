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
        let request = self.provider.assemble(request, self.workspace.as_ref()).await;
        self.inner.execute(request, sink, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use harness_protocol::ids::{BackendId, RequestId, RunId};
    use harness_protocol::usage::{Cost, ModelUsage};
    use harness_runtime::workspace::FakeWorkspace;

    struct RecordingProvider {
        received_prompt: Mutex<Option<String>>,
        rewritten_prompt: String,
    }

    #[async_trait]
    impl ContextProvider for RecordingProvider {
        async fn assemble(&self, mut request: ExecutionRequest, _workspace: &dyn Workspace) -> ExecutionRequest {
            *self.received_prompt.lock().unwrap() = Some(request.system_prompt.clone());
            request.system_prompt = self.rewritten_prompt.clone();
            request
        }
    }

    /// Records every request it receives and returns a canned success —
    /// exists purely to prove `ContextAssemblingBackend` hands the *rewritten*
    /// request to the inner backend, not the original.
    struct RecordingBackend {
        seen: Mutex<Vec<ExecutionRequest>>,
    }

    #[async_trait]
    impl ExecutionBackend for RecordingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: BackendId::new(),
                name: "recording".to_string(),
                description: "test double".to_string(),
                capabilities: BackendCapabilities::default(),
            }
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn execute(
            &self,
            request: ExecutionRequest,
            _sink: broadcast::Sender<ExecutionEvent>,
            _cancel: CancellationToken,
        ) -> Result<ExecutionResult, ExecutionError> {
            self.seen.lock().unwrap().push(request.clone());
            Ok(ExecutionResult {
                request_id: request.request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".to_string(),
            })
        }
    }

    fn request(system_prompt: &str) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: system_prompt.to_string(),
            messages: vec![],
            tools: vec![],
            extended_thinking: false,
        }
    }

    #[tokio::test]
    async fn provider_rewrites_request_before_inner_backend_sees_it() {
        let provider = Arc::new(RecordingProvider {
            received_prompt: Mutex::new(None),
            rewritten_prompt: "assembled prompt".to_string(),
        });
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapped = ContextAssemblingBackend::new(
            backend.clone(),
            provider.clone(),
            Arc::new(FakeWorkspace::new()),
        );

        let (tx, _rx) = broadcast::channel(16);
        wrapped
            .execute(request("original prompt"), tx, CancellationToken::new())
            .await
            .expect("execute should succeed");

        assert_eq!(
            *provider.received_prompt.lock().unwrap(),
            Some("original prompt".to_string())
        );
        let seen = backend.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].system_prompt, "assembled prompt");
    }
}

//! [`ContextAssemblingBackend`]: wraps an `ExecutionBackend`, running every
//! request through a [`ContextProvider`] first.
//!
//! Mirrors `ToolAdvertisingBackend` in
//! `crates/rusty-engine/src/session_builder.rs`, which wraps a backend to
//! rewrite `request.tools` before delegating — this is the identical
//! decorator shape applied to `system_prompt`/`messages` instead, and is why
//! context assembly needs zero changes to `rusty-agent-core` or `agent_runner.rs`:
//! it's purely a backend-wrapping concern applied at
//! `SessionBuilder::start()` time (see `rusty-engine`'s
//! `SessionBuilder::context_provider`).

use std::sync::Arc;

use async_trait::async_trait;
use rusty_protocol::events::AgentEvent;
use rusty_runtime::traits::{
    ExecutionBackend, RequestObservationBindings, RequestObservationPublisher, Workspace,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::policy::ContextPolicy;
use crate::provider::{
    ContextObservation, ContextObservationOrigin, ContextObservationPublisher, ContextProvider,
    TrustedModelWindow,
};
use rusty_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};

pub struct ContextAssemblingBackend {
    inner: Arc<dyn ExecutionBackend>,
    provider: Arc<dyn ContextProvider>,
    workspace: Arc<dyn Workspace>,
    observation_bindings: RequestObservationBindings,
    /// The session's model and its catalogue window, when the host resolved both.
    trusted_model_window: Option<TrustedModelWindow>,
    policy: ContextPolicy,
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
            observation_bindings: RequestObservationBindings::default(),
            trusted_model_window: None,
            policy: ContextPolicy::default(),
        }
    }

    pub fn with_observation_bindings(mut self, bindings: RequestObservationBindings) -> Self {
        self.observation_bindings = bindings;
        self
    }

    /// The window of the session's resolved model. An estimate is reported only for requests
    /// that run on this model; without it every request's estimate is unavailable.
    pub fn with_trusted_model_window(mut self, window: Option<TrustedModelWindow>) -> Self {
        self.trusted_model_window = window;
        self
    }

    /// The observation for a request the runner bound at dispatch, if it did.
    fn observation(&self, request: &ExecutionRequest) -> Option<ContextObservation> {
        let binding = self.observation_bindings.get(request.request_id)?;
        Some(ContextObservation {
            origin: ContextObservationOrigin {
                session_id: binding.session_id,
                agent_id: binding.agent_id,
                parent_agent_id: binding.parent_agent_id,
                run_id: binding.run_id,
                request_id: binding.request_id,
            },
            request_start_order: binding.request_start_order,
            trusted_model_window: self.trusted_model_window.clone(),
            session_cancellation: binding.session_cancellation.clone(),
            publisher: Arc::new(PublisherAdapter {
                publisher: binding.publisher.clone(),
                run_id: binding.run_id,
            }),
        })
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

    fn observation_bindings(&self) -> Option<RequestObservationBindings> {
        Some(self.observation_bindings.clone())
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let observation = self.observation(&request);
        let assembly = async {
            match &observation {
                Some(observation) => {
                    self.provider
                        .assemble_with_observation(request, self.workspace.as_ref(), observation)
                        .await
                }
                None => {
                    self.provider
                        .assemble(request, self.workspace.as_ref())
                        .await
                }
            }
        };
        // A cancelled request must not wait for a slow provider: its dispatch binding is released
        // when this returns.
        let mut request = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ExecutionError::Cancelled),
            request = assembly => request,
        };
        request.messages = rusty_protocol::messages::repair_tool_history(request.messages);
        // The estimate describes the request exactly as it goes to the model.
        if let Some(observation) = &observation {
            let update = crate::visibility::context_update(&request, observation, self.policy);
            observation.publisher.publish(update).await;
        }
        self.inner.execute(request, sink, cancel).await
    }
}

struct PublisherAdapter {
    publisher: Arc<dyn RequestObservationPublisher>,
    run_id: rusty_protocol::ids::RunId,
}

#[async_trait]
impl ContextObservationPublisher for PublisherAdapter {
    async fn publish(&self, event: AgentEvent) -> bool {
        self.publisher.publish(event, self.run_id).await
    }
}

#[cfg(test)]
mod tests;

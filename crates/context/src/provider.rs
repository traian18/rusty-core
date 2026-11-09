//! [`ContextProvider`]: the async, workspace-aware counterpart of
//! `rusty-agent-core`'s pure `Agent::execution_request` (see
//! `crates/rusty-agent-core/src/transitions.rs`), which builds every
//! [`ExecutionRequest`] straight from `AgentState::system_prompt` /
//! `messages` with no injection or compaction. `Agent::apply` is documented
//! as a deterministic, synchronous state machine, so context assembly that
//! reads live workspace state (or, eventually, calls a model for
//! summarization) cannot live there — it runs one layer up, wrapping the
//! [`ExecutionBackend`](rusty_runtime::traits::ExecutionBackend) instead.

use std::sync::Arc;

use async_trait::async_trait;
use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::ids::{AgentId, RequestId, RunId, SessionId};
use rusty_runtime::traits::Workspace;
use tokio_util::sync::CancellationToken;

/// Identifies the request that owns an observation.
#[derive(Clone)]
pub struct ContextObservationOrigin {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub run_id: RunId,
    pub request_id: RequestId,
}

/// A model window associated with its trusted resolved model identity.
#[derive(Clone)]
pub struct TrustedModelWindow {
    pub model: String,
    pub model_window: u64,
}

/// Request-bound information available to observation-aware providers.
#[derive(Clone)]
pub struct ContextObservation {
    pub origin: ContextObservationOrigin,
    pub request_start_order: u64,
    pub trusted_model_window: Option<TrustedModelWindow>,
    pub session_cancellation: CancellationToken,
    pub publisher: Arc<dyn ContextObservationPublisher>,
}

/// Publishes events for the origin to which the publisher is bound.
#[async_trait]
pub trait ContextObservationPublisher: Send + Sync {
    /// Returns false if publication is rejected or unavailable.
    async fn publish(&self, event: AgentEvent) -> bool;
}

/// Rewrites an outgoing [`ExecutionRequest`]'s `system_prompt`/`messages`
/// before it reaches the backend.
///
/// Implementations receive the session's [`Workspace`] so they can pull in
/// live project state (file listings, a project instructions file) without
/// a tool round-trip.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest;

    /// Assemble with request-bound observation data, if supported by the provider.
    async fn assemble_with_observation(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
        _observation: &ContextObservation,
    ) -> ExecutionRequest {
        self.assemble(request, workspace).await
    }
}

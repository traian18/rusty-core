//! Core runtime traits for execution backends and event sinks, and the
//! request observation bindings a backend publishes context observations through.
//!
//! These are the **behavioral** traits that give life to the wire-format types
//! defined in `rusty-protocol`.  The protocol crate holds only serializable
//! data structures ([`ExecutionRequest`], [`ExecutionEvent`],
//! [`ToolCall`](rusty_protocol::tools::ToolCall),
//! [`AgentEventEnvelope`], etc.); the traits here drive the actual async I/O,
//! cancellation, and streaming that implement those contracts.
//!
//! # Migrated types
//!
//! Tool executor, registry, and workspace types have been moved to the
//! `rusty-tool-api` and `rusty-workspace` crates respectively.  This
//! module re-exports them for backward compatibility during the migration
//! period.  New code should import directly from the owning crate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusty_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult as ProtocolExecutionResult,
};
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope};
use rusty_protocol::ids::{AgentId, RequestId, RunId, SessionId};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

// Backward-compat re-exports: tool-executor and registry types live in `rusty-tool-api`,
// workspace types in `rusty-workspace`.
pub use rusty_tool_api::registry::{RegistrationError, SimpleToolRegistry, ToolRegistry};
pub use rusty_tool_api::{
    CancellationToken, ExecutionFailure, ExecutionResult, FailureKind, ProgressPhase,
    ToolDescriptor, ToolExecutor, ToolId, ToolInput, ToolProgress, ToolUsage,
};
pub use rusty_workspace::{
    FileInfo, SearchResult, ToolResult, Workspace, WorkspaceError, WorkspaceMode,
};

/// Publishes a request's context observations as agent events of the run that made the
/// request, even when they arrive after that run settled.
#[async_trait]
pub trait RequestObservationPublisher: Send + Sync {
    /// Publishes `event` for `run_id`; false when it could not be published (the session closed).
    async fn publish(&self, event: AgentEvent, run_id: RunId) -> bool;
}

/// Who made a model request, and where its observations go: the agent runner registers one
/// before the request reaches the backend, and the context backend looks it up by request id.
#[derive(Clone)]
pub struct RequestObservationBinding {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub run_id: RunId,
    pub request_id: RequestId,
    /// The order the session's requests started in, so a reader can tell which is newest.
    pub request_start_order: u64,
    pub publisher: Arc<dyn RequestObservationPublisher>,
    /// Cancelled when the session ends; work the request started (a background summary) stops then.
    pub session_cancellation: TokioCancellationToken,
}

/// A session's bindings by request id. Clones share one registry; each session has its own.
#[derive(Clone, Default)]
pub struct RequestObservationBindings(Arc<Mutex<HashMap<RequestId, RequestObservationBinding>>>);

impl RequestObservationBindings {
    pub fn register(&self, binding: RequestObservationBinding) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(binding.request_id, binding);
    }

    pub fn get(&self, request_id: RequestId) -> Option<RequestObservationBinding> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&request_id)
            .cloned()
    }

    pub(crate) fn remove_if_publisher(
        &self,
        request_id: RequestId,
        publisher: &Arc<dyn RequestObservationPublisher>,
    ) {
        let mut bindings = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if bindings
            .get(&request_id)
            .is_some_and(|binding| Arc::ptr_eq(&binding.publisher, publisher))
        {
            bindings.remove(&request_id);
        }
    }

    pub fn remove(&self, request_id: RequestId) -> Option<RequestObservationBinding> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&request_id)
    }
}

/// An execution backend that processes agent requests against a language model.
///
/// Implementations wrap a specific provider (Anthropic, OpenAI, etc.) and are
/// responsible for:
///
/// * Serializing the [`ExecutionRequest`] into the provider's wire format.
/// * Streaming back normalized [`ExecutionEvent`]s (text deltas, reasoning,
///   tool call requests, usage updates, final result, or errors).
/// * Observing the cancellation token so that in-flight requests can be
///   aborted promptly.
///
/// # Object safety
///
/// This trait is `dyn`-compatible — all methods take `&self` and return owned
/// values or futures.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Returns the backend's descriptor, including its identity and capabilities.
    fn descriptor(&self) -> BackendDescriptor;

    /// Returns a summary of the backend's capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// Returns the session's request observation bindings when supported.
    fn observation_bindings(&self) -> Option<RequestObservationBindings> {
        None
    }

    /// Execute a request against this backend.
    ///
    /// The backend streams [`ExecutionEvent`]s into `sink` as they occur
    /// and returns the final [`ExecutionResult`] once execution completes
    /// (or an [`ExecutionError`] if something went wrong).
    ///
    /// # Cancellation
    ///
    /// When `cancel` is triggered the backend should stop work as soon as
    /// practical and return [`ExecutionError::Cancelled`].
    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: TokioCancellationToken,
    ) -> Result<ProtocolExecutionResult, ExecutionError>;
}

/// A sink for receiving agent event envelopes.
///
/// Implementations are responsible for fanning events out to session-level
/// subscribers, persisting them, or forwarding them to external observers.
/// The canonical implementation publishes onto a
/// [`broadcast::Sender<AgentEventEnvelope>`] but custom sinks may layer on
/// filtering, encryption, or audit logging.
///
/// # Object safety
///
/// This trait is `dyn`-compatible.
pub trait EventSink: Send + Sync {
    /// Send an event envelope for processing.
    ///
    /// Implementations **should not** block indefinitely; if the underlying
    /// channel is full the implementation should decide whether to drop the
    /// event, buffer it, or apply backpressure.
    fn send(&self, event: AgentEventEnvelope);
}

#[cfg(test)]
mod observation_binding_tests {
    use super::*;

    struct Publisher;

    #[async_trait]
    impl RequestObservationPublisher for Publisher {
        async fn publish(&self, _event: AgentEvent, _run_id: RunId) -> bool {
            true
        }
    }

    #[test]
    fn registry_clones_share_bindings_but_sessions_are_isolated() {
        let bindings = RequestObservationBindings::default();
        let session_clone = bindings.clone();
        let other_session = RequestObservationBindings::default();
        let request_id = RequestId::new();
        bindings.register(RequestObservationBinding {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: RunId::new(),
            request_id,
            request_start_order: 7,
            publisher: Arc::new(Publisher),
            session_cancellation: TokioCancellationToken::new(),
        });
        assert_eq!(
            session_clone.get(request_id).map(|b| b.request_start_order),
            Some(7)
        );
        assert!(other_session.get(request_id).is_none());
        session_clone.remove(request_id);
        assert!(bindings.get(request_id).is_none());
    }
}

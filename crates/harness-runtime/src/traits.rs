//! Core runtime traits for execution backends, tool executors, tool registries,
//! workspaces, and event sinks.
//!
//! These are the **behavioral** traits that give life to the wire-format types
//! defined in [`harness-protocol`].  The protocol crate holds only serializable
//! data structures ([`ExecutionRequest`], [`ExecutionEvent`], [`ToolCall`],
//! [`AgentEventEnvelope`], etc.); the traits here drive the actual async I/O,
//! cancellation, and streaming that implement those contracts.
//!
//! # Migrated types
//!
//! Tool executor, registry, and workspace types have been moved to the
//! [`harness-tools`] and [`harness-workspace`] crates respectively.  This
//! module re-exports them for backward compatibility during the migration
//! period.  New code should import directly from the owning crate.

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult as ProtocolExecutionResult,
};
use harness_protocol::events::AgentEventEnvelope;

// ---------------------------------------------------------------------------
// Backward-compat re-exports from harness-tools
// ---------------------------------------------------------------------------
//
// Tool-executor and registry types now live in `harness-tools`.
pub use harness_tools::{
    CancellationToken, ExecutionFailure, ExecutionResult, FailureKind, ProgressPhase,
    ToolDescriptor, ToolExecutor, ToolId, ToolInput, ToolProgress, ToolUsage,
};
pub use harness_tools::registry::{RegistrationError, SimpleToolRegistry, ToolRegistry};

// ---------------------------------------------------------------------------
// Backward-compat re-exports from harness-workspace
// ---------------------------------------------------------------------------
//
// Workspace types now live in `harness-workspace`.
pub use harness_workspace::FileInfo;
pub use harness_workspace::SearchResult;
pub use harness_workspace::ToolResult;
pub use harness_workspace::Workspace;
pub use harness_workspace::WorkspaceError;
pub use harness_workspace::WorkspaceMode;

// ---------------------------------------------------------------------------
// ExecutionBackend
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// EventSink
// ---------------------------------------------------------------------------

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
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Send an event envelope for processing.
    ///
    /// Implementations **should not** block indefinitely; if the underlying
    /// channel is full the implementation should decide whether to drop the
    /// event, buffer it, or apply backpressure.
    fn send(&self, envelope: AgentEventEnvelope);
}

//! Provider-neutral model event, result, and error types.
//!
//! These types are consumed by [`ModelClient`] implementations and by the
//! harness runtime when processing streaming model responses.
//!
//! [`ModelClient`]: crate::client::ModelClient

use harness_protocol::ids::ToolCallId;
use harness_protocol::usage::{Cost, ModelUsage};
use thiserror::Error;

/// Events emitted during a streaming model invocation.
///
/// The `ToolCall*` variants support streaming tool call JSON fragments
/// from providers such as Anthropic.
#[derive(Debug, Clone)]
pub enum ModelEvent {
    /// A text delta from the model.
    TextDelta {
        delta: String,
    },
    /// A reasoning / thinking delta from the model.
    ReasoningDelta {
        delta: String,
    },
    /// A tool call has been started — the tool name and ID are now known.
    ToolCallStarted {
        id: ToolCallId,
        name: String,
    },
    /// A partial JSON fragment for an in-progress tool call.
    ToolCallDelta {
        id: ToolCallId,
        delta: String,
    },
    /// A tool call has been completed with its full input payload.
    ToolCallCompleted {
        id: ToolCallId,
        name: String,
        input: serde_json::Value,
    },
    /// An intermediate usage update (may be emitted before [`Completed`]).
    ///
    /// [`Completed`]: ModelEvent::Completed
    UsageUpdate {
        usage: ModelUsage,
    },
    /// The model response completed successfully.
    Completed {
        result: ModelResult,
    },
    /// The model response terminated with an error.
    Error {
        error: ModelError,
    },
}

/// The result of a completed model invocation.
#[derive(Debug, Clone, Default)]
pub struct ModelResult {
    /// The reason the model stopped (e.g. `"end_turn"`, `"max_tokens"`, `"tool_use"`).
    pub stop_reason: String,
    /// Token usage accumulated during the invocation.
    pub usage: ModelUsage,
    /// Financial cost of this invocation, if the [`ModelClient`](crate::client::ModelClient)
    /// implementation is able to compute or report one.
    ///
    /// Provider-neutral by construction: this field carries whatever
    /// [`Cost`] the provider-specific client computed (e.g. via a
    /// per-model rate table) or reported. `GenericModelBackend` passes it
    /// through unchanged into `ExecutionResult::cost`.
    pub cost: Cost,
}

/// Errors that can occur during a model invocation.
#[derive(Debug, Clone, Error)]
pub enum ModelError {
    /// The backend returned an error response.
    #[error("backend error ({code}): {message}")]
    BackendError {
        /// Human-readable error message.
        message: String,
        /// Provider-specific error code.
        code: String,
    },
    /// The request was rate-limited by the backend.
    #[error("rate limited")]
    RateLimited {
        /// Number of seconds to wait before retrying, if advertised by the provider.
        retry_after: Option<u64>,
    },
    /// The request was malformed or otherwise invalid.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Details about what made the request invalid.
        message: String,
    },
    /// The request was cancelled (e.g. by the caller dropping the receiver).
    #[error("cancelled")]
    Cancelled,
    /// The request timed out.
    #[error("timeout")]
    Timeout,
    /// A protocol-level error (e.g. deserialization failure, unexpected wire format).
    #[error("protocol error: {message}")]
    Protocol {
        /// Description of the protocol error.
        message: String,
    },
}

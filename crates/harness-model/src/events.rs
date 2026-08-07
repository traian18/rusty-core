//! Provider-neutral model event, result, and error types.
//!
//! These types are consumed by [`ModelClient`] implementations and by the
//! harness runtime when processing streaming model responses.
//!
//! [`ModelClient`]: crate::client::ModelClient

use harness_protocol::ids::ToolCallId;
use harness_protocol::usage::{Cost, ModelUsage};
use std::time::Duration;
use thiserror::Error;

/// Events emitted during a streaming model invocation.
///
/// The `ToolCall*` variants support streaming tool call JSON fragments
/// from providers such as Anthropic.
#[derive(Debug, Clone)]
pub enum ModelEvent {
    /// A text delta from the model.
    TextDelta { delta: String },
    /// A reasoning / thinking delta from the model.
    ReasoningDelta { delta: String },
    /// A tool call has been started — the tool name and ID are now known.
    ToolCallStarted { id: ToolCallId, name: String },
    /// A partial JSON fragment for an in-progress tool call.
    ToolCallDelta { id: ToolCallId, delta: String },
    /// A tool call has been completed with its full input payload.
    ToolCallCompleted {
        id: ToolCallId,
        name: String,
        input: serde_json::Value,
    },
    /// An intermediate usage update (may be emitted before [`Completed`]).
    ///
    /// [`Completed`]: ModelEvent::Completed
    UsageUpdate { usage: ModelUsage },
    /// The model response completed successfully.
    Completed { result: ModelResult },
    /// The model response terminated with an error.
    Error { error: ModelError },
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
        /// Normalized delay to wait before retrying, if advertised by the provider.
        retry_after: Option<Duration>,
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
    /// The provider is temporarily unavailable because its circuit is open.
    #[error("provider unavailable; retry after {retry_after:?}")]
    CircuitOpen {
        /// Remaining time before a probe request is permitted.
        retry_after: Duration,
    },
    /// A protocol-level error (e.g. deserialization failure, unexpected wire format).
    #[error("protocol error: {message}")]
    Protocol {
        /// Description of the protocol error.
        message: String,
    },
    /// The request asked for a capability (reasoning, images, a specific
    /// param) the target model/provider does not support. Raised *before*
    /// any network call is made, so it never causes a billed request.
    #[error("unsupported capability: {capability}")]
    UnsupportedCapability {
        /// Machine-readable name of the unsupported capability
        /// (e.g. `"reasoning"`, `"images"`).
        capability: String,
        /// Human-readable detail (e.g. which model/provider was asked).
        detail: String,
    },
}

impl ModelError {
    /// Whether retrying the unchanged request may recover from this error.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Timeout => true,
            Self::BackendError { code, .. } => {
                code == "request_failed"
                    || code == "408"
                    || code
                        .parse::<u16>()
                        .is_ok_and(|status| (500..600).contains(&status))
            }
            Self::InvalidRequest { .. }
            | Self::Cancelled
            | Self::CircuitOpen { .. }
            | Self::Protocol { .. }
            | Self::UnsupportedCapability { .. } => false,
        }
    }

    /// A provider-advertised delay, when available.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModelError;
    use std::time::Duration;

    #[test]
    fn retryability_is_limited_to_transient_failures() {
        assert!(ModelError::Timeout.is_retryable());
        assert!(ModelError::RateLimited { retry_after: None }.is_retryable());
        assert!(ModelError::BackendError {
            message: String::new(),
            code: "503".into()
        }
        .is_retryable());
        assert!(!ModelError::InvalidRequest {
            message: String::new()
        }
        .is_retryable());
        assert!(!ModelError::Protocol {
            message: String::new()
        }
        .is_retryable());
        assert!(!ModelError::CircuitOpen {
            retry_after: Duration::from_secs(1)
        }
        .is_retryable());
    }
}

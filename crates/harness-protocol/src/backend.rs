//! Backend descriptor, reference, binding, and execution types for the harness protocol.
//!
//! This module defines how backends are described (capabilities, identity),
//! how they are referenced for persistence, how execution requests are structured,
//! and how execution events and results flow back to the agent.

use serde::{Deserialize, Serialize};

use crate::ids::{
    BackendId, ConfigurationId, IntegrationId, ModelId, RequestId, RunId,
};

use crate::messages::AgentMessage;
use crate::tools::{ToolCall, ToolDescriptor};
use crate::usage::{Cost, ModelUsage};

// ---------------------------------------------------------------------------
// Backend identity & capabilities
// ---------------------------------------------------------------------------

/// Full descriptor of a backend, including its identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendDescriptor {
    /// Unique identifier for this backend instance.
    pub id: BackendId,
    /// Human-readable name (e.g. "Anthropic", "Claude Code").
    pub name: String,
    /// A short description of the backend's purpose.
    pub description: String,
    /// The set of capabilities this backend supports.
    pub capabilities: BackendCapabilities,
}

/// Declares which optional features a backend supports.
///
/// This is the primary mechanism for capability-based dispatch.
/// Consumers must **never** branch on backend identity;
/// they should inspect these boolean flags instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendCapabilities {
    /// Whether the backend can stream partial response deltas.
    pub streaming: bool,
    /// Whether the backend exposes a separate reasoning/thinking stream.
    pub reasoning_stream: bool,
    /// Whether the backend supports tool/function calls.
    pub tool_calls: bool,
    /// Whether the backend can issue multiple tool calls in one response.
    pub parallel_tool_calls: bool,
    /// Whether the *host* (harness runtime) manages tool execution.
    pub host_managed_tools: bool,
    /// Whether the *backend* itself manages tool execution.
    pub backend_managed_tools: bool,
    /// Whether the backend has a permission/approval system.
    pub permissions: bool,
    /// Whether the backend supports image inputs.
    pub images: bool,
    /// Whether the backend can resume interrupted sessions.
    pub resumable_sessions: bool,
    /// Whether the backend has native subagent support.
    pub native_subagents: bool,
    /// Whether the model can be switched mid-session.
    pub model_switching: bool,
    /// Whether the backend reports exact token usage.
    pub exact_usage: bool,
    /// Whether the backend reports exact cost information.
    pub exact_cost: bool,
}

// ---------------------------------------------------------------------------
// Backend reference & binding
// ---------------------------------------------------------------------------

/// A persistable reference to a backend configuration.
///
/// This is what gets stored in session/agent state so the runtime can
/// reconstruct a concrete [`ExecutionBackend`] at restore time.
///
/// Credentials are **never** stored in this reference; they belong in
/// the integration configuration store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendReference {
    /// Which integration family (e.g. `"anthropic"`, `"codex"`).
    pub integration: IntegrationId,
    /// Which saved configuration (e.g. `"work-account"`, `"personal"`).
    pub configuration: ConfigurationId,
    /// Optional model override within that integration.
    pub model: Option<ModelId>,
}

/// Versioned, secret-free provider selection persisted with a backend reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBackendSelection {
    pub version: u8,
    pub provider: String,
    pub credential_profile: String,
    pub provider_model_id: String,
}

impl PersistedBackendSelection {
    pub fn v1(provider: impl Into<String>, credential_profile: impl Into<String>, provider_model_id: impl Into<String>) -> Self {
        Self { version: 1, provider: provider.into(), credential_profile: credential_profile.into(), provider_model_id: provider_model_id.into() }
    }
}

/// A full backend binding combining a persistable reference and a live descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendBinding {
    /// The persistable reference to the backend configuration.
    pub reference: BackendReference,
    /// The live descriptor, used by the agent to inspect capabilities.
    pub descriptor: BackendDescriptor,
}

// ---------------------------------------------------------------------------
// Execution request & context
// ---------------------------------------------------------------------------

/// Coarse, provider-neutral reasoning effort level.
///
/// Providers map this onto their own mechanism: Anthropic maps any non-`None`
/// level onto `extended_thinking` with a scaled token budget; OpenAI's
/// reasoning models (`o1`/`o3`/...) pass it through directly as
/// `reasoning_effort`. A model/provider that doesn't support reasoning at all
/// rejects a non-`None` request with a typed unsupported-capability error
/// rather than silently ignoring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

/// Model selection and sampling/output parameters for one execution request.
///
/// Every field is optional/empty by default, meaning "use the provider's own
/// default" — this struct only ever carries explicit overrides. It doubles as
/// both a resolved value (on [`ExecutionRequest`]) and a partial update (via
/// [`ExecutionParams::merge_over`]): a `None`/empty field in an update means
/// "leave whatever was set before," not "clear it." `stop_sequences` can't
/// distinguish "explicitly clear" from "not specified" under this scheme —
/// an accepted simplification for now; a caller that wants to clear stop
/// sequences currently cannot express that as a partial update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionParams {
    /// Provider-specific model identifier override (e.g. `"claude-opus-4-20250514"`).
    pub model: Option<String>,
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<u64>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Sequences that stop generation when encountered.
    pub stop_sequences: Vec<String>,
    /// Coarse reasoning effort; `None` means no explicit reasoning request.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether extended thinking/reasoning was explicitly requested.
    /// `None` means "not specified" (falls back to `false` at request time),
    /// distinct from an explicit `Some(false)`.
    pub extended_thinking: Option<bool>,
    /// Provider-specific options that don't have a provider-neutral
    /// equivalent, namespaced by provider id (e.g.
    /// `{"anthropic": {"top_k": 40}}`). Providers that don't recognize their
    /// namespace's contents ignore it rather than erroring — this is meant
    /// for advanced/experimental knobs, not required behavior.
    pub provider_options: serde_json::Value,
}

impl ExecutionParams {
    /// Applies `update` over `self`, keeping `self`'s value for any field
    /// `update` leaves unset. Used both to apply a session-level
    /// `ConfigureExecution` command and (in the future) a per-run override.
    pub fn merge_over(&self, update: &ExecutionParams) -> ExecutionParams {
        ExecutionParams {
            model: update.model.clone().or_else(|| self.model.clone()),
            max_tokens: update.max_tokens.or(self.max_tokens),
            temperature: update.temperature.or(self.temperature),
            stop_sequences: if update.stop_sequences.is_empty() {
                self.stop_sequences.clone()
            } else {
                update.stop_sequences.clone()
            },
            reasoning_effort: update.reasoning_effort.or(self.reasoning_effort),
            extended_thinking: update.extended_thinking.or(self.extended_thinking),
            provider_options: if update.provider_options.is_null() {
                self.provider_options.clone()
            } else {
                update.provider_options.clone()
            },
        }
    }
}

/// A complete request to be sent to an execution backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    /// Unique identifier for this request.
    pub request_id: RequestId,
    /// The run this request belongs to.
    pub run_id: RunId,
    /// The system prompt / instruction for the agent.
    pub system_prompt: String,
    /// The conversation history so far.
    pub messages: Vec<AgentMessage>,
    /// The tools the agent is allowed to use.
    pub tools: Vec<ToolDescriptor>,
    /// Whether to request extended thinking / reasoning from the backend.
    /// Kept alongside `params.extended_thinking` for source compatibility
    /// with existing readers; always equal to
    /// `params.extended_thinking.unwrap_or(false)` when constructed via
    /// `harness-core`.
    pub extended_thinking: bool,
    /// Model selection and sampling/output parameters. See [`ExecutionParams`].
    #[serde(default)]
    pub params: ExecutionParams,
}

/// Parameters that control how a backend executes a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<u64>,
    /// Sampling temperature (0.0–1.0 or higher).
    pub temperature: Option<f64>,
    /// Sequences that will stop generation when encountered.
    pub stop_sequences: Vec<String>,
}

// ---------------------------------------------------------------------------
// Execution result
// ---------------------------------------------------------------------------

/// The final result of a backend execution request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// The request this result belongs to.
    pub request_id: RequestId,
    /// Token usage for this execution.
    pub usage: ModelUsage,
    /// Financial cost of this execution, if known.
    pub cost: Cost,
    /// Reason why execution finished (e.g. "end_turn", "max_tokens", "stop").
    pub finish_reason: String,
}

// ---------------------------------------------------------------------------
// Execution error
// ---------------------------------------------------------------------------

/// Errors that can occur during backend execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionError {
    /// A general backend error occurred.
    BackendError {
        /// Human-readable error message.
        message: String,
        /// Machine-readable error code.
        code: String,
    },
    /// The request was rate-limited by the backend.
    RateLimited {
        /// How long to wait before retrying, if known.
        retry_after: Option<u64>,
    },
    /// The request was invalid and cannot be retried as-is.
    InvalidRequest {
        /// Explanation of what was invalid.
        message: String,
    },
    /// The execution was cancelled.
    Cancelled,
    /// The execution timed out.
    Timeout,
    /// The request asked for a capability the target model/provider does not
    /// support (e.g. reasoning on a non-reasoning model, images on a
    /// text-only model). Raised before any network call, so it never causes
    /// a billed request — see `GenericModelBackend::execute`.
    UnsupportedCapability {
        /// Machine-readable capability name (e.g. `"reasoning"`, `"images"`).
        capability: String,
        /// Human-readable detail.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Execution event
// ---------------------------------------------------------------------------

/// An event emitted during backend execution.
///
/// These events are consumed by the agent's transition function and
/// represent the normalized interface between backends and the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionEvent {
    /// A delta of assistant text output.
    TextDelta {
        /// The request this event belongs to.
        request_id: RequestId,
        /// The incremental text chunk.
        delta: String,
    },
    /// A delta of reasoning/thinking content (backend-dependent).
    ReasoningDelta {
        /// The request this event belongs to.
        request_id: RequestId,
        /// The incremental reasoning chunk.
        delta: String,
    },
    /// A tool call was requested by the model.
    ToolCallRequested {
        /// The request this event belongs to.
        request_id: RequestId,
        /// The tool call details.
        call: ToolCall,
    },
    /// An update on token usage so far.
    UsageUpdate {
        /// The request this event belongs to.
        request_id: RequestId,
        /// Accumulated usage so far.
        usage: ModelUsage,
    },
    /// Execution completed successfully.
    Completed {
        /// The request this event belongs to.
        request_id: RequestId,
        /// The final result.
        result: ExecutionResult,
    },
    /// Execution failed with an error.
    Error {
        /// The request this event belongs to.
        request_id: RequestId,
        /// The error that occurred.
        error: ExecutionError,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_default_to_disabled() {
        let capabilities = BackendCapabilities::default();
        assert!(!capabilities.streaming);
        assert!(!capabilities.tool_calls);
        assert!(!capabilities.exact_cost);
    }

    #[test]
    fn execution_params_merge_keeps_base_fields_the_update_leaves_unset() {
        let base = ExecutionParams {
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(4096),
            temperature: Some(0.7),
            stop_sequences: vec!["STOP".to_string()],
            reasoning_effort: None,
            extended_thinking: None,
            provider_options: serde_json::Value::Null,
        };
        let update = ExecutionParams {
            temperature: Some(0.2),
            ..Default::default()
        };
        let merged = base.merge_over(&update);
        // The update's explicit field wins...
        assert_eq!(merged.temperature, Some(0.2));
        // ...everything the update left unset carries over from base.
        assert_eq!(merged.model, base.model);
        assert_eq!(merged.max_tokens, base.max_tokens);
        assert_eq!(merged.stop_sequences, base.stop_sequences);
    }

    #[test]
    fn execution_params_merge_overrides_model_and_reasoning() {
        let base = ExecutionParams::default();
        let update = ExecutionParams {
            model: Some("gpt-4.1".to_string()),
            reasoning_effort: Some(ReasoningEffort::High),
            extended_thinking: Some(true),
            ..Default::default()
        };
        let merged = base.merge_over(&update);
        assert_eq!(merged.model.as_deref(), Some("gpt-4.1"));
        assert_eq!(merged.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(merged.extended_thinking, Some(true));
    }

    #[test]
    fn execution_params_default_is_all_unset() {
        let params = ExecutionParams::default();
        assert!(params.model.is_none());
        assert!(params.max_tokens.is_none());
        assert!(params.temperature.is_none());
        assert!(params.stop_sequences.is_empty());
        assert!(params.reasoning_effort.is_none());
        assert!(params.extended_thinking.is_none());
    }

    #[test]
    fn persisted_backend_selection_roundtrips() {
        let selection =
            PersistedBackendSelection::v1("openai-api", "openai-api:default", "gpt-test");
        let encoded = serde_json::to_string(&selection).expect("serialize selection");
        let decoded: PersistedBackendSelection =
            serde_json::from_str(&encoded).expect("deserialize selection");
        assert_eq!(decoded, selection);
    }

    #[test]
    fn execution_event_roundtrips() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::TextDelta {
            request_id,
            delta: "hello".into(),
        };
        let encoded = serde_json::to_string(&event).expect("serialize event");
        let decoded: ExecutionEvent = serde_json::from_str(&encoded).expect("deserialize event");
        assert!(matches!(
            decoded,
            ExecutionEvent::TextDelta { request_id: id, delta }
                if id == request_id && delta == "hello"
        ));
    }
}

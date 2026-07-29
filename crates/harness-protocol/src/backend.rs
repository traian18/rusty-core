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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for BackendCapabilities {
    fn default() -> Self {
        Self {
            streaming: false,
            reasoning_stream: false,
            tool_calls: false,
            parallel_tool_calls: false,
            host_managed_tools: false,
            backend_managed_tools: false,
            permissions: false,
            images: false,
            resumable_sessions: false,
            native_subagents: false,
            model_switching: false,
            exact_usage: false,
            exact_cost: false,
        }
    }
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
    pub extended_thinking: bool,
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
    use crate::ids::BackendId;

    // -----------------------------------------------------------------------
    // Capability-based branching
    // -----------------------------------------------------------------------

    /// Demonstrates that capability-based dispatch compiles and works correctly.
    #[test]
    fn capability_branching_works() {
        let caps = BackendCapabilities {
            streaming: true,
            tool_calls: true,
            ..Default::default()
        };

        // Simulate capability-based dispatch
        let use_streaming = caps.streaming;
        let use_tools = caps.tool_calls;
        let use_reasoning = caps.reasoning_stream;
        let use_images = caps.images;

        assert!(use_streaming, "streaming should be enabled");
        assert!(use_tools, "tool_calls should be enabled");
        assert!(!use_reasoning, "reasoning_stream should be disabled by default");
        assert!(!use_images, "images should be disabled by default");

        // All-zeros default
        let default_caps = BackendCapabilities::default();
        assert!(!default_caps.streaming);
        assert!(!default_caps.exact_cost);
        assert!(!default_caps.resumable_sessions);
    }

    /// Verifies that a backend descriptor round-trips through JSON with
    /// a mix of enabled and disabled capabilities.
    #[test]
    fn backend_descriptor_roundtrip() {
        let desc = BackendDescriptor {
            id: BackendId::new(),
            name: "test-backend".into(),
            description: "A test backend".into(),
            capabilities: BackendCapabilities {
                streaming: true,
                tool_calls: true,
                parallel_tool_calls: true,
                host_managed_tools: true,
                ..Default::default()
            },
        };

        let json = serde_json::to_string(&desc).expect("serialize descriptor");
        let deserialized: BackendDescriptor =
            serde_json::from_str(&json).expect("deserialize descriptor");

        assert_eq!(desc.id, deserialized.id);
        assert_eq!(desc.name, deserialized.name);
        assert_eq!(desc.capabilities.streaming, deserialized.capabilities.streaming);
        assert_eq!(desc.capabilities.tool_calls, deserialized.capabilities.tool_calls);
        assert_eq!(
            desc.capabilities.parallel_tool_calls,
            deserialized.capabilities.parallel_tool_calls
        );
        assert!(!deserialized.capabilities.reasoning_stream);
        assert!(!deserialized.capabilities.exact_cost);
    }

    // -----------------------------------------------------------------------
    // BackendReference / BackendBinding
    // -----------------------------------------------------------------------

    #[test]
    fn backend_reference_roundtrip() {
        let reference = BackendReference {
            integration: IntegrationId::new(),
            configuration: ConfigurationId::new(),
            model: None,
        };

        let json = serde_json::to_string(&reference).expect("serialize reference");
        let deserialized: BackendReference =
            serde_json::from_str(&json).expect("deserialize reference");

        assert_eq!(reference.integration, deserialized.integration);
        assert_eq!(reference.configuration, deserialized.configuration);
        assert!(deserialized.model.is_none());
    }

    #[test]
    fn backend_binding_roundtrip() {
        let binding = BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: Some(ModelId::new()),
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "production".into(),
                description: "Production backend".into(),
                capabilities: BackendCapabilities {
                    streaming: true,
                    ..Default::default()
                },
            },
        };

        let json = serde_json::to_string(&binding).expect("serialize binding");
        let deserialized: BackendBinding =
            serde_json::from_str(&json).expect("deserialize binding");

        assert_eq!(binding.reference.integration, deserialized.reference.integration);
        assert_eq!(
            binding.descriptor.name,
            deserialized.descriptor.name
        );
        assert!(deserialized.reference.model.is_some());
    }

    // -----------------------------------------------------------------------
    // ExecutionRequest / ExecutionContext
    // -----------------------------------------------------------------------

    #[test]
    fn execution_request_roundtrip() {
        let request = ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: "You are a helpful assistant.".into(),
            messages: vec![],
            tools: vec![],
            extended_thinking: false,
        };

        let json = serde_json::to_string(&request).expect("serialize request");
        let deserialized: ExecutionRequest =
            serde_json::from_str(&json).expect("deserialize request");

        assert_eq!(request.request_id, deserialized.request_id);
        assert_eq!(request.run_id, deserialized.run_id);
        assert_eq!(request.system_prompt, deserialized.system_prompt);
        assert!(!deserialized.extended_thinking);
    }

    #[test]
    fn execution_context_defaults() {
        let ctx = ExecutionContext {
            max_tokens: Some(4096),
            temperature: Some(0.7),
            stop_sequences: vec![],
        };

        assert_eq!(ctx.max_tokens, Some(4096));
        assert!((ctx.temperature.unwrap() - 0.7).abs() < f64::EPSILON);
        assert!(ctx.stop_sequences.is_empty());
    }

    // -----------------------------------------------------------------------
    // ExecutionEvent round-trip serialization
    // -----------------------------------------------------------------------

    #[test]
    fn execution_event_text_delta_roundtrip() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::TextDelta {
            request_id,
            delta: "Hello".into(),
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::TextDelta {
                request_id: rid,
                delta,
            } => {
                assert_eq!(rid, request_id);
                assert_eq!(delta, "Hello");
            }
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn execution_event_reasoning_delta_roundtrip() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::ReasoningDelta {
            request_id,
            delta: "thinking...".into(),
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::ReasoningDelta {
                request_id: rid,
                delta,
            } => {
                assert_eq!(rid, request_id);
                assert_eq!(delta, "thinking...");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }
    }

    #[test]
    fn execution_event_tool_call_requested_roundtrip() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::ToolCallRequested {
            request_id,
            call: ToolCall {
                id: crate::ids::ToolCallId::new(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "rust"}),
            },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::ToolCallRequested {
                request_id: rid,
                call,
            } => {
                assert_eq!(rid, request_id);
                assert_eq!(call.name, "search");
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }

    #[test]
    fn execution_event_usage_update_roundtrip() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::UsageUpdate {
            request_id,
            usage: ModelUsage::default(),
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::UsageUpdate {
                request_id: rid,
                usage,
            } => {
                assert_eq!(rid, request_id);
                assert!(usage.input_tokens.is_unknown());
            }
            other => panic!("expected UsageUpdate, got {other:?}"),
        }
    }

    #[test]
    fn execution_event_completed_roundtrip() {
        let request_id = RequestId::new();
        let usage = ModelUsage::default();
        let result = ExecutionResult {
            request_id,
            usage,
            cost: Cost {
                amount_usd: None,
                source: None,
            },
            finish_reason: "end_turn".into(),
        };
        let event = ExecutionEvent::Completed {
            request_id,
            result,
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::Completed {
                request_id: rid,
                result: res,
            } => {
                assert_eq!(rid, request_id);
                assert_eq!(res.finish_reason, "end_turn");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn execution_event_error_roundtrip() {
        let request_id = RequestId::new();
        let event = ExecutionEvent::Error {
            request_id,
            error: ExecutionError::RateLimited { retry_after: Some(30) },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: ExecutionEvent =
            serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ExecutionEvent::Error {
                request_id: rid,
                error,
            } => {
                assert_eq!(rid, request_id);
                match error {
                    ExecutionError::RateLimited { retry_after } => {
                        assert_eq!(retry_after, Some(30));
                    }
                    other => panic!("expected RateLimited, got {other:?}"),
                }
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Round-trip all six ExecutionEvent variants through a single serialization
    /// cycle to verify the enum discriminant is preserved.
    #[test]
    fn all_execution_event_variants_roundtrip() {
        let request_id = RequestId::new();
        let events: Vec<ExecutionEvent> = vec![
            ExecutionEvent::TextDelta {
                request_id,
                delta: "a".into(),
            },
            ExecutionEvent::ReasoningDelta {
                request_id,
                delta: "b".into(),
            },
            ExecutionEvent::ToolCallRequested {
                request_id,
                call: ToolCall {
                    id: crate::ids::ToolCallId::new(),
                    name: "c".into(),
                    arguments: serde_json::json!({}),
                },
            },
            ExecutionEvent::UsageUpdate {
                request_id,
                usage: ModelUsage::default(),
            },
            ExecutionEvent::Completed {
                request_id,
                result: ExecutionResult {
                    request_id,
                    usage: ModelUsage::default(),
                    cost: Cost {
                        amount_usd: None,
                        source: None,
                    },
                    finish_reason: "d".into(),
                },
            },
            ExecutionEvent::Error {
                request_id,
                error: ExecutionError::Cancelled,
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).expect("serialize");
            let deserialized: ExecutionEvent =
                serde_json::from_str(&json).expect("deserialize");
            // Verify discriminant matches by comparing debug representation
            let expected_tag = std::mem::discriminant(event);
            let actual_tag = std::mem::discriminant(&deserialized);
            assert_eq!(
                expected_tag, actual_tag,
                "discriminant mismatch for event: {event:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // ExecutionError
    // -----------------------------------------------------------------------

    #[test]
    fn execution_error_variants_roundtrip() {
        let errors = vec![
            ExecutionError::BackendError {
                message: "bad request".into(),
                code: "ERR_400".into(),
            },
            ExecutionError::RateLimited { retry_after: Some(60) },
            ExecutionError::InvalidRequest {
                message: "missing field".into(),
            },
            ExecutionError::Cancelled,
            ExecutionError::Timeout,
        ];

        for error in &errors {
            let json = serde_json::to_string(error).expect("serialize");
            let deserialized: ExecutionError =
                serde_json::from_str(&json).expect("deserialize");
            let expected_tag = std::mem::discriminant(error);
            let actual_tag = std::mem::discriminant(&deserialized);
            assert_eq!(
                expected_tag, actual_tag,
                "discriminant mismatch for error: {error:?}"
            );
        }
    }
}

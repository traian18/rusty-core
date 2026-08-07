//! Provider-neutral model request and capabilities types.
//!
//! These types are a subset of the backend-level protocol types:
//! - [`ModelCapabilities`] mirrors `BackendCapabilities` but omits cost-related fields.
//! - [`ModelRequest`] is a subset of `ExecutionRequest` that strips out `run_id` / `request_id`
//!   (those are backend-level concerns).

use serde::{Deserialize, Serialize};

use harness_protocol::backend::ReasoningEffort;
use harness_protocol::messages::AgentMessage;
use harness_protocol::tools::ToolDescriptor;

/// Static capabilities advertised by a model provider.
///
/// Mirrors `BackendCapabilities` without cost-related fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub reasoning: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    /// Whether the provider accepts image content blocks in messages.
    pub images: bool,
}

/// A request to be sent to a model provider.
///
/// This is a subset of `ExecutionRequest` that excludes `run_id` / `request_id`
/// because those identifiers are a backend-level concern and should not leak into
/// the model layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<ToolDescriptor>,
    pub model: Option<String>,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub extended_thinking: bool,
    /// Coarse reasoning effort; `None` means no explicit request. Providers
    /// that don't support reasoning at all are expected to have already been
    /// rejected by `GenericModelBackend`'s capability check before a
    /// `ModelClient` ever sees this — see `ModelError::UnsupportedCapability`.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Provider-specific options namespaced by provider id. See
    /// `harness_protocol::backend::ExecutionParams::provider_options`.
    pub provider_options: serde_json::Value,
}

//! Retry budgets and run-wide resource and research policies.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    #[serde(default)]
    pub retry_on: Vec<RetryReason>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            retry_on: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    BackendRateLimited,
    BackendTimeout,
    ToolTimeout,
    InvalidStructuredOutput,
    VerificationFailed,
    /// The user reviewed a result and asked for changes. Raised by approval
    /// steps; never needs to be listed in a target's `retry_on`.
    ChangesRequested,
}

/// Upper bound on any node's `retry.max_attempts`.
pub const MAX_STEP_ATTEMPTS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrchestrationPolicies {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_elapsed_ms: Option<u64>,
    /// Longest an active step may go without any sign of life (agent event,
    /// usage report, checkpoint) before the runner cancels the attempt and
    /// fails it as retryable. Time spent waiting on a permission decision or
    /// while paused does not count. Must exceed the longest legitimate silent
    /// operation, e.g. a tool call (`tool_call_timeout`) or a model's first
    /// event (`first_event_timeout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    /// On-demand research for agent steps; omitted, the defaults apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<ResearchPolicy>,
}

/// An agent step that needs information its inputs do not hold may send the
/// question to a read-only research step, which runs on the spot (with the
/// asking step's read tools, never more) and returns its findings. Every
/// step that may read files can ask, whether or not the flow declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchPolicy {
    /// Questions one attempt may send; 0 turns research off.
    #[serde(default = "default_research_per_attempt")]
    pub max_per_attempt: u32,
    /// The profile research runs under; omitted, the session's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<crate::behavior::ProfileRef>,
}

impl Default for ResearchPolicy {
    fn default() -> Self {
        Self {
            max_per_attempt: default_research_per_attempt(),
            profile: None,
        }
    }
}

const fn default_research_per_attempt() -> u32 {
    2
}

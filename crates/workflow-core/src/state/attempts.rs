//! One try at a step: its status, what it did, and what it cost.

use serde::{Deserialize, Serialize};

use super::run::is_zero;
use serde_json::Value;

use super::OrchestrationError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepAttempt {
    pub attempt: u32,
    pub status: AttemptStatus,
    pub output: Option<Value>,
    pub error: Option<OrchestrationError>,
    #[serde(default)]
    pub details: AttemptDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Observations the runtime reports about a finished attempt. The reducer
/// records them verbatim; timestamps come from the runtime because the core
/// performs no I/O (including reading the clock).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttemptDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// The exact bound input the step executed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated: Option<DelegatedRunRef>,
    /// The resolved tool allow list for an agent attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub usage: UsageSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// What an agent attempt did, read from its tool calls.
    #[serde(default, skip_serializing_if = "StepActions::is_empty")]
    pub actions: StepActions,
}

/// What an agent attempt did, recorded from its tool calls rather than from
/// what the model says it did. A later attempt at the same step, and the
/// steps after it, read it to pick up where the work stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepActions {
    /// Files it wrote or edited, in the order first touched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_changed: Vec<String>,
    /// Files it read, in the order first read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_read: Vec<String>,
    /// Commands and project checks it ran, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandRun>,
    /// Questions it sent out for research.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research: Vec<String>,
    /// Where its research found things (`path:lines` evidence lines), so the
    /// steps after it read those lines instead of searching again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    /// Tool calls that returned an error.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_errors: u32,
}

impl StepActions {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRun {
    /// The command line, or the check's name (`test`, `typecheck`, ...).
    pub command: String,
    pub failed: bool,
}

/// Correlation to the session/agent/run that executed a delegated attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedRunRef {
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_run_id: Option<String>,
}

/// A recorded claim about why a criterion is (or is not) satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub check: String,
    pub passed: bool,
    pub detail: String,
}

/// Resource consumption. Unknown token counts and costs are tracked as
/// unknown, never as zero: `tokens` and `cost_usd` sum only known values and
/// the `*_unknown` flags record that at least one contribution was missing,
/// so the sums are lower bounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub model_requests: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    #[serde(default)]
    pub tokens_unknown: bool,
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_unknown: bool,
}

impl UsageSummary {
    pub fn add(&mut self, other: &UsageSummary) {
        self.model_requests += other.model_requests;
        self.tool_calls += other.tool_calls;
        self.tokens += other.tokens;
        self.tokens_unknown |= other.tokens_unknown;
        self.cost_usd += other.cost_usd;
        self.cost_unknown |= other.cost_unknown;
    }
}

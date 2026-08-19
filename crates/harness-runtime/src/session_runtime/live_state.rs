//! Live, continuously-updated per-agent status/usage projections.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rust_decimal::Decimal;

use harness_core::usage::AgentUsageSummary as CoreAgentUsageSummary;
use harness_protocol::commands::{AgentError, AgentOperation, AgentStatus};
use harness_protocol::events::AgentOutcome;
use harness_protocol::ids::AgentId;

// ---------------------------------------------------------------------------
// AgentLiveState — live read projection of one agent's runtime status
// ---------------------------------------------------------------------------

/// A live, continuously-updated projection of one agent's runtime status,
/// operation, outcome, and usage.
///
/// Every [`AgentRunner`](crate::agent_runner::AgentRunner) publishes its
/// current state here immediately after each
/// [`Agent::apply`](harness_core::agent::Agent) transition, so
/// [`SessionRuntime::agent_live_state`](super::SessionRuntime::agent_live_state)
/// always returns a pure read of live state rather than a stale copy
/// captured at construction time.
#[derive(Debug, Clone)]
pub struct AgentLiveState {
    /// The agent's current [`AgentStatus`].
    pub status: AgentStatus,
    /// What the agent is currently doing, if anything more specific than
    /// `status` alone conveys.
    pub current_operation: Option<AgentOperation>,
    /// The outcome of the most recently completed run, if any.
    ///
    /// `AgentStatus` returns to `Idle` after a successful run completes, so
    /// this field is the only durable signal that a run finished
    /// successfully versus never having run at all.
    pub last_outcome: Option<AgentOutcome>,
    /// The most recent unrecoverable error, if any.
    pub last_error: Option<AgentError>,
    /// Self/descendant/inclusive token usage aggregated from the agent's
    /// usage ledger.
    pub usage: CoreAgentUsageSummary,
    /// Total number of usage records recorded so far (approximates request
    /// count for Phase 2's single-backend-call-per-record model).
    pub total_requests: u64,
    /// Total cost across all usage records that reported one, if any
    /// reported a cost at all.
    pub total_cost_usd: Option<Decimal>,
}

impl Default for AgentLiveState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            current_operation: None,
            last_outcome: None,
            last_error: None,
            usage: CoreAgentUsageSummary::default(),
            total_requests: 0,
            total_cost_usd: None,
        }
    }
}

/// Shared table of per-agent [`AgentLiveState`], written by
/// [`AgentRunner`](crate::agent_runner::AgentRunner)s and read by
/// [`SessionRuntime::agent_live_state`](super::SessionRuntime::agent_live_state) /
/// `SessionClient::snapshot`.
pub type LiveStateTable = Arc<Mutex<HashMap<AgentId, AgentLiveState>>>;

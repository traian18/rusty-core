//! Event types for the harness protocol.
//!
//! This module defines [`EventVisibility`], [`AgentEventEnvelope`], [`AgentEvent`],
//! and [`AgentOutcome`] — the observable events that an agent emits throughout
//! its lifecycle.  Events are wrapped in an envelope that carries routing and
//! ordering metadata.

use serde::{Deserialize, Serialize};

use crate::commands::{AgentError, AgentStatus};
use crate::effects::PermissionRequest;
use crate::ids::{AgentId, EventId, MessageId, RequestId, RunId, SessionId, Timestamp, ToolCallId};
use crate::tools::{ToolCall, ToolProgress, ToolResultSummary};
use crate::usage::AgentUsageSnapshot;

// ---------------------------------------------------------------------------
// EventVisibility
// ---------------------------------------------------------------------------

/// Controls which audience an event is visible to.
///
/// - [`User`](EventVisibility::User) — visible to end-users (shown in the UI).
/// - [`Developer`](EventVisibility::Developer) — visible to developers/extensions.
/// - [`Internal`](EventVisibility::Internal) — used for telemetry and logging only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventVisibility {
    /// Visible to end-users (shown in the UI).
    User,
    /// Visible to developers/extensions.
    Developer,
    /// Used for telemetry and logging only.
    Internal,
}

// ---------------------------------------------------------------------------
// AgentOutcome
// ---------------------------------------------------------------------------

/// The final result of an agent run or child-agent execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentOutcome {
    /// Agent completed its work successfully.
    Success,
    /// Agent was cancelled before completing.
    Cancelled,
    /// Agent encountered an unrecoverable error.
    Failed,
}

// ---------------------------------------------------------------------------
// AgentEventEnvelope
// ---------------------------------------------------------------------------

/// A fully qualified event emitted by an agent.
///
/// Every observable occurrence in the harness is wrapped in this envelope,
/// providing routing metadata (`session_id`, `agent_id`, `parent_agent_id`),
/// temporal context (`timestamp`, `run_id`), and ordering primitives.
///
/// ## Ordering
///
/// [`agent_sequence`](AgentEventEnvelope::agent_sequence) and
/// [`session_sequence`](AgentEventEnvelope::session_sequence) are **monotonic
/// counters** that clients should use to order events from a single agent or
/// session.  Do **not** rely on timestamps alone for ordering, especially
/// when multiple agents are executing concurrently — two events from different
/// agents may have identical timestamps but a well-defined causal order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    /// Unique identifier for this event (enables deduplication and replay).
    pub event_id: EventId,
    /// The session this event belongs to.
    pub session_id: SessionId,
    /// The agent that produced this event.
    pub agent_id: AgentId,
    /// The parent agent of the producing agent, if any (root agents have `None`).
    pub parent_agent_id: Option<AgentId>,
    /// The run this event is associated with, if any.
    pub run_id: Option<RunId>,
    /// Monotonic counter scoped to the producing agent.  Strictly increasing
    /// per agent — used to order events emitted by the same agent.
    pub agent_sequence: u64,
    /// Monotonic counter scoped to the entire session.  Present on events
    /// that flow through the session bus; `None` for agent-local events that
    /// have not yet been committed to the session stream.
    pub session_sequence: Option<u64>,
    /// Wall-clock timestamp of when the event was produced.
    pub timestamp: Timestamp,
    /// Controls which audience this event is visible to.
    pub visibility: EventVisibility,
    /// The event payload.
    pub event: AgentEvent,
}

// ---------------------------------------------------------------------------
// AgentEvent
// ---------------------------------------------------------------------------

/// The payload of an event emitted by an agent.
///
/// Each variant represents a distinct occurrence in the agent's lifecycle:
/// state transitions, backend interactions, tool calls, permission requests,
/// child-agent activity, and final outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Opt-in diagnostic at the common dispatch boundary. Never durable in
    /// the session store; hosts must redact before persisting trajectories.
    ModelRequestPrepared {
        request: Box<crate::backend::ExecutionRequest>,
    },
    /// The agent's status changed (e.g. `Idle` → `PreparingContext`).
    StateChanged {
        /// The previous status.
        from: AgentStatus,
        /// The new status.
        to: AgentStatus,
    },

    /// A new run was started on this agent.
    RunStarted {
        /// The identifier of the newly created run.
        run_id: RunId,
    },

    /// A request was sent to the execution backend.
    BackendRequestStarted {
        /// The identifier of the backend request.
        request_id: RequestId,
    },

    /// The backend started producing a new assistant message.
    AssistantMessageStarted {
        /// The identifier of the assistant message.
        message_id: MessageId,
    },

    /// A text delta was received from the streaming backend for
    /// an in-progress assistant message.
    AssistantTextDelta {
        /// The message being streamed.
        message_id: MessageId,
        /// The incremental text chunk.
        delta: String,
    },

    /// A reasoning/thinking delta was received from the backend.
    ReasoningDelta {
        /// The message being streamed.
        message_id: MessageId,
        /// The incremental reasoning chunk.
        delta: String,
    },

    /// An assistant message finished streaming.
    AssistantMessageCompleted {
        /// The identifier of the completed message.
        message_id: MessageId,
    },

    /// The model requested a tool call.
    ToolCallRequested {
        /// The tool call details.
        call: ToolCall,
    },

    /// A tool call execution has started.
    ToolCallStarted {
        /// The identifier of the tool call being executed.
        call_id: ToolCallId,
    },

    /// Progress update for a long-running tool call.
    ToolCallProgress {
        /// The identifier of the tool call.
        call_id: ToolCallId,
        /// The current progress of the tool execution.
        progress: ToolProgress,
    },

    /// A tool call completed successfully.
    ToolCallCompleted {
        /// The identifier of the completed tool call.
        call_id: ToolCallId,
        /// A summary of the tool's result.
        result: ToolResultSummary,
    },

    /// The agent is waiting for a user permission decision.
    PermissionRequested {
        /// The permission request that needs resolution.
        request: PermissionRequest,
    },

    /// An update on the agent's token usage and cost.
    UsageUpdated {
        /// A snapshot of the agent's current usage.
        usage: AgentUsageSnapshot,
    },

    /// A child agent was spawned.
    ChildAgentSpawned {
        /// The identifier of the child agent.
        agent_id: AgentId,
        /// The parent's `agent.spawn` tool call that created this child, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<ToolCallId>,
    },

    /// A child agent completed its run.
    ChildAgentCompleted {
        /// The identifier of the child agent.
        agent_id: AgentId,
        /// The outcome of the child's run.
        outcome: AgentOutcome,
    },

    /// The agent encountered an unrecoverable error.
    Failed {
        /// Details about the error.
        error: AgentError,
    },

    /// The agent completed its run with the given outcome.
    Completed {
        /// The final outcome of the run.
        outcome: AgentOutcome,
    },

    /// A behavior profile rule fired.
    BehaviorRuleFired {
        /// The active profile, as `id@revision`.
        profile: String,
        rule_id: String,
        /// The loop event, e.g. `PreToolUse`.
        event: String,
        /// The action taken, e.g. `deny` or `inject`.
        action: String,
    },

    /// Harness-authored context was added for the model.
    ContextInjected {
        /// What produced it, e.g. `profile:reviewer@2 rule:test-after-edit`.
        source: String,
        /// `with_result`, `next_request`, or `persistent`.
        placement: String,
        /// Length of the injected text in characters.
        chars: u64,
    },

    /// The harness refused a tool call before execution.
    ToolCallDenied {
        call_id: ToolCallId,
        /// The rule that denied it; `None` for scope or limit denials.
        rule_id: Option<String>,
        reason: String,
    },

    /// The completion gate checked a proposed final answer.
    CompletionGateEvaluated {
        /// Evaluation number within the run, starting at 1.
        attempt: u32,
        passed: bool,
        /// `true` when the rejection was sent back and the run continues.
        continuing: bool,
        /// Ids of the checks that failed.
        failed_checks: Vec<String>,
    },

    /// The agent's behavior profile changed.
    ProfileChanged {
        /// Previous profile, as `id@revision`.
        from: String,
        /// New profile, as `id@revision`.
        to: String,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

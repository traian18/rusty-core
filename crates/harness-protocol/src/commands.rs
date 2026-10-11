//! Command types for the harness protocol.
//!
//! This module defines [`AgentCommand`] — the set of messages that can be
//! sent to an agent to drive its state machine — together with the supporting
//! types that appear in those commands: user input, agent results, permission
//! decisions, errors, status, and operation descriptions.

use serde::{Deserialize, Serialize};

use crate::backend::ExecutionEvent;
use crate::effects::SpawnAgentSpec;
use crate::ids::{AgentId, PermissionId, RunId, ToolCallId};
use crate::tools::{ToolError, ToolResult};
use crate::usage::AgentUsageSummary;

// ---------------------------------------------------------------------------
// UserInput, Attachment
// ---------------------------------------------------------------------------

/// Input provided by an end-user to start or continue a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInput {
    /// The textual content of the user's prompt.
    pub text: String,
    /// Any file or media attachments included with the input.
    pub attachments: Vec<Attachment>,
}

/// A file or media attachment provided as part of user input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// MIME type of the attachment (e.g. `"text/plain"`, `"image/png"`).
    pub mime_type: String,
    /// Raw bytes of the attachment content.
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// AgentResult, PermissionDecision, AgentError
// ---------------------------------------------------------------------------

/// The outcome of a completed agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// A human-readable summary of what the agent accomplished.
    pub summary: String,
    /// Token usage and cost for the run.
    pub usage: AgentUsageSummary,
    /// Whether the run's completion gate passed. `None` when the agent's
    /// profile has no gate; `Some(false)` when the run finished (accepted)
    /// with checks still failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_passed: Option<bool>,
}

/// The user's decision in response to a permission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// The requested action is permitted.
    Approved,
    /// The requested action is denied.
    Denied,
}

/// Describes an error that occurred during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentError {
    /// A human-readable error message.
    pub message: String,
    /// A machine-readable error code (e.g. `"TOOL_FAILED"`, `"CHILD_FAILED"`).
    pub code: String,
    /// Optional structured details that provide more context about the error.
    pub details: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// AgentStatus
// ---------------------------------------------------------------------------

/// The high-level status of an agent at any point in time.
///
/// This is the primary mechanism for frontends to display concise agent state.
/// For more detail about what the agent is currently doing, see
/// [`AgentOperation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentStatus {
    /// Agent is ready to accept a new command.
    Idle,
    /// Agent is assembling context (system prompt, tools, conversation history)
    /// before sending a request to the backend.
    PreparingContext,
    /// Agent has sent a request to the execution backend and is waiting for a
    /// response stream.
    WaitingForBackend,
    /// Agent is receiving streaming output from the backend.
    Streaming,
    /// Agent is executing one or more tool calls (or waiting for tool results).
    Executing,
    /// Agent is waiting for a user permission decision before proceeding.
    WaitingForPermission,
    /// Agent has spawned children and is waiting for them to complete.
    WaitingForChildren,
    /// The model proposed a final answer; the completion gate is checking it.
    Verifying,
    /// Agent has been explicitly paused.
    Paused,
    /// Agent has completed its run successfully.
    Completed,
    /// Agent was cancelled before completing.
    Cancelled,
    /// Agent encountered an unrecoverable error.
    Failed,
}

// ---------------------------------------------------------------------------
// AgentOperation
// ---------------------------------------------------------------------------

/// Describes what an agent is currently doing, providing more detail than
/// [`AgentStatus`] alone.
///
/// Frontends can use this to display a richer progress indicator such as
/// "Making a backend request…", "Running 3 tools…", or "Waiting for child…".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentOperation {
    /// The agent is waiting for a response from the execution backend.
    BackendRequest {
        /// The identifier of the pending request.
        request_id: crate::ids::RequestId,
    },
    /// The agent is waiting for one or more tool calls to complete.
    Tools {
        /// The identifiers of the outstanding tool calls.
        calls: Vec<ToolCallId>,
    },
    /// The agent is waiting for one or more child agents to complete.
    Children {
        /// The identifiers of the outstanding child agents.
        agents: Vec<AgentId>,
    },
    /// The agent is waiting for a user permission decision.
    Permission {
        /// The identifier of the pending permission request.
        request_id: PermissionId,
    },
}

// ---------------------------------------------------------------------------
// AgentCommand
// ---------------------------------------------------------------------------

/// A command sent to an agent to drive its state machine.
///
/// Each variant corresponds to an external event or user action that the
/// agent's transition function (`Agent::apply`) processes, returning a
/// list of [`AgentEffect`](crate::effects::AgentEffect)s.
// `SpawnChild`'s `SpawnAgentSpec` makes this enum ~464 bytes, so every
// command — including one-byte ones like `Cancel` and `Pause` — costs that
// much in a channel buffer. `SessionCommand`, which carries the same spec,
// already carries this same allow for the same reason. Boxing
// `SpawnAgentSpec` is the real fix (it is wire-compatible: `Box<T>`
// serializes identically to `T`), but it ripples into
// `AgentEffect::SpawnAgent` and the deterministic core's transition
// function, so it is tracked as its own change rather than folded into an
// unrelated one. See PRODUCTION_READINESS.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum AgentCommand {
    /// Start a new run with the given user input.
    StartRun { input: UserInput },

    /// Inject additional user input into the conversation.
    ///
    /// Runtimes that are unable to interrupt an in-flight backend request
    /// must preserve ordering and deliver this command after that request has
    /// reached a command boundary.
    Steer { input: UserInput },

    /// Queue user input to begin the next run after the current run reaches
    /// a command boundary.
    FollowUp { input: UserInput },

    /// Start the next already-admitted FIFO input after the preceding run's
    /// terminal effects have been published. This is runtime-internal; public
    /// clients should use [`Self::FollowUp`] instead.
    StartNextQueuedRun,

    /// An event arrived from the execution backend for the active run.
    BackendEvent {
        /// Which run this event belongs to.
        run_id: RunId,
        /// The normalized backend execution event.
        event: ExecutionEvent,
    },

    /// A tool call completed successfully.
    ToolCompleted {
        /// The identifier of the completed tool call.
        call_id: ToolCallId,
        /// The result produced by the tool.
        result: ToolResult,
    },

    /// A tool call failed.
    ToolFailed {
        /// The identifier of the failed tool call.
        call_id: ToolCallId,
        /// The error that occurred.
        error: ToolError,
    },

    /// A permission request was resolved by the user.
    PermissionResolved {
        /// The identifier of the permission request that was resolved.
        id: PermissionId,
        /// The user's decision.
        decision: PermissionDecision,
    },

    /// Ask the runtime to create a child according to this policy.
    SpawnChild { spec: SpawnAgentSpec },

    /// A child was successfully created and registered by the runtime.
    ChildSpawned {
        agent_id: AgentId,
        /// Whether the parent must pause until this child terminates.
        awaiting: bool,
    },

    /// A child agent completed its run successfully.
    ChildCompleted {
        /// The identifier of the child agent.
        agent_id: AgentId,
        /// The result produced by the child.
        result: AgentResult,
    },

    /// A child agent failed.
    ChildFailed {
        /// The identifier of the child agent.
        agent_id: AgentId,
        /// The error that occurred.
        error: AgentError,
    },

    /// Cancel the current run immediately.
    Cancel,

    /// Pause the current run (can be resumed later).
    Pause,

    /// Resume a previously paused run.
    Resume,

    /// Update the agent's session-level default execution params (model,
    /// max_tokens, temperature, reasoning, ...).
    ///
    /// Applied as a partial update via `ExecutionParams::merge_over` — fields
    /// left unset in `params` keep their previous value. Takes effect
    /// starting with this agent's *next* model request -- including the next
    /// one within an active run (e.g. after its pending tools finish); it
    /// never mutates an already-in-flight request. This is the only mutation path for
    /// execution params — both "set the session default at creation" and
    /// "override for the next prompt" go through this same command, sent
    /// immediately before `StartRun`/`Steer`/`FollowUp` for the latter case.
    ConfigureExecution {
        params: crate::backend::ExecutionParams,
    },

    /// Replace the agent's behavior profile (a full profile document, see
    /// `harness_core::behavior`). Applied immediately when the agent has no
    /// active run, otherwise when the next run starts. Hosts resolve and
    /// validate the document against their profile registry before sending.
    SetBehaviorProfile {
        profile: serde_json::Value,
        /// Every profile `profile` can reach through `switch_profile`
        /// rules, resolved by the host. The agent switches only within it.
        #[serde(default)]
        library: Vec<serde_json::Value>,
        /// Whether these profiles may run shell commands (`command`
        /// evaluators). Hosts grant this only to profiles they trust.
        #[serde(default)]
        allow_commands: bool,
    },

    /// Verdicts for a completion evaluation (`AgentEffect::EvaluateCompletion`).
    CompletionEvaluated {
        run_id: crate::ids::RunId,
        attempt: u32,
        verdicts: Vec<CheckVerdict>,
        /// Model requests made by evaluators, counted as the agent's usage.
        #[serde(default)]
        usage: Vec<crate::usage::UsageRecord>,
    },
}

/// The result of one completion-gate evaluator check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckVerdict {
    pub id: String,
    pub outcome: VerdictOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictOutcome {
    Pass,
    Fail {
        feedback: String,
    },
    /// The evaluator could not produce a verdict.
    Error {
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// starting with the *next* run this agent starts; it never mutates an
    /// already-in-flight request. This is the only mutation path for
    /// execution params — both "set the session default at creation" and
    /// "override for the next prompt" go through this same command, sent
    /// immediately before `StartRun`/`Steer`/`FollowUp` for the latter case.
    ConfigureExecution {
        params: crate::backend::ExecutionParams,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ExecutionEvent;
    use crate::ids::{AgentId, PermissionId, RunId, ToolCallId};

    /// Round-trip JSON serialization of `AgentCommand::StartRun`.
    #[test]
    fn start_run_roundtrip() {
        let cmd = AgentCommand::StartRun {
            input: UserInput {
                text: "Hello".into(),
                attachments: vec![Attachment {
                    mime_type: "text/plain".into(),
                    data: b"hello world".to_vec(),
                }],
            },
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::StartRun { input } => {
                assert_eq!(input.text, "Hello");
                assert_eq!(input.attachments.len(), 1);
                assert_eq!(input.attachments[0].mime_type, "text/plain");
            }
            other => panic!("expected StartRun, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::BackendEvent`.
    #[test]
    fn backend_event_roundtrip() {
        let cmd = AgentCommand::BackendEvent {
            run_id: RunId::new(),
            event: ExecutionEvent::TextDelta {
                request_id: crate::ids::RequestId::new(),
                delta: "test delta".into(),
            },
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::BackendEvent { run_id: _, event } => match event {
                ExecutionEvent::TextDelta { delta, .. } => {
                    assert_eq!(delta, "test delta");
                }
                other => panic!("expected TextDelta, got {other:?}"),
            },
            other => panic!("expected BackendEvent, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::ToolCompleted`.
    #[test]
    fn tool_completed_roundtrip() {
        let cmd = AgentCommand::ToolCompleted {
            call_id: ToolCallId::new(),
            result: ToolResult {
                call_id: ToolCallId::new(),
                output: serde_json::json!({"key": "value"}),
                is_error: false,
            },
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::ToolCompleted { result, .. } => {
                assert!(!result.is_error);
            }
            other => panic!("expected ToolCompleted, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::ToolFailed`.
    #[test]
    fn tool_failed_roundtrip() {
        let cmd = AgentCommand::ToolFailed {
            call_id: ToolCallId::new(),
            error: ToolError::Timeout,
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::ToolFailed { error, .. } => {
                assert!(matches!(error, ToolError::Timeout));
            }
            other => panic!("expected ToolFailed, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::PermissionResolved`.
    #[test]
    fn permission_resolved_roundtrip() {
        let cmd = AgentCommand::PermissionResolved {
            id: PermissionId::new(),
            decision: PermissionDecision::Approved,
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::PermissionResolved { decision, .. } => {
                assert!(matches!(decision, PermissionDecision::Approved));
            }
            other => panic!("expected PermissionResolved, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::ChildCompleted`.
    #[test]
    fn child_completed_roundtrip() {
        let cmd = AgentCommand::ChildCompleted {
            agent_id: AgentId::new(),
            result: AgentResult {
                summary: "done".into(),
                usage: AgentUsageSummary::default(),
            },
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::ChildCompleted { result, .. } => {
                assert_eq!(result.summary, "done");
            }
            other => panic!("expected ChildCompleted, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentCommand::ChildFailed`.
    #[test]
    fn child_failed_roundtrip() {
        let cmd = AgentCommand::ChildFailed {
            agent_id: AgentId::new(),
            error: AgentError {
                message: "something went wrong".into(),
                code: "ERR_INTERNAL".into(),
                details: Some(serde_json::json!({"reason": "timeout"})),
            },
        };

        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            AgentCommand::ChildFailed { error, .. } => {
                assert_eq!(error.message, "something went wrong");
                assert_eq!(error.code, "ERR_INTERNAL");
                assert!(error.details.is_some());
            }
            other => panic!("expected ChildFailed, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of unit variants.
    #[test]
    fn unit_variants_roundtrip() {
        for cmd in [
            AgentCommand::Cancel,
            AgentCommand::Pause,
            AgentCommand::Resume,
            AgentCommand::StartNextQueuedRun,
        ] {
            let json = serde_json::to_string(&cmd).expect("serialize");
            let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");
            let expected_tag = std::mem::discriminant(&cmd);
            let actual_tag = std::mem::discriminant(&deserialized);
            assert_eq!(
                expected_tag, actual_tag,
                "discriminant mismatch for {cmd:?}"
            );
        }
    }

    /// Round-trip JSON serialization of `PermissionDecision::Denied`.
    #[test]
    fn permission_decision_denied_roundtrip() {
        let decision = PermissionDecision::Denied;
        let json = serde_json::to_string(&decision).expect("serialize");
        let deserialized: PermissionDecision = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(deserialized, PermissionDecision::Denied));
    }

    /// Round-trip JSON serialization of `AgentStatus`.
    #[test]
    fn agent_status_roundtrip() {
        let statuses = [
            AgentStatus::Idle,
            AgentStatus::PreparingContext,
            AgentStatus::WaitingForBackend,
            AgentStatus::Streaming,
            AgentStatus::Executing,
            AgentStatus::WaitingForPermission,
            AgentStatus::WaitingForChildren,
            AgentStatus::Paused,
            AgentStatus::Completed,
            AgentStatus::Cancelled,
            AgentStatus::Failed,
        ];

        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let deserialized: AgentStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, deserialized);
        }
    }

    /// Round-trip JSON serialization of `AgentOperation`.
    #[test]
    fn agent_operation_backend_request_roundtrip() {
        let op = AgentOperation::BackendRequest {
            request_id: crate::ids::RequestId::new(),
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            AgentOperation::BackendRequest { .. } => {}
            other => panic!("expected BackendRequest, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentOperation::Tools`.
    #[test]
    fn agent_operation_tools_roundtrip() {
        let op = AgentOperation::Tools {
            calls: vec![ToolCallId::new(), ToolCallId::new()],
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            AgentOperation::Tools { calls } => {
                assert_eq!(calls.len(), 2);
            }
            other => panic!("expected Tools, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentOperation::Children`.
    #[test]
    fn agent_operation_children_roundtrip() {
        let op = AgentOperation::Children {
            agents: vec![AgentId::new()],
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            AgentOperation::Children { agents } => {
                assert_eq!(agents.len(), 1);
            }
            other => panic!("expected Children, got {other:?}"),
        }
    }

    /// Round-trip JSON serialization of `AgentOperation::Permission`.
    #[test]
    fn agent_operation_permission_roundtrip() {
        let op = AgentOperation::Permission {
            request_id: PermissionId::new(),
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            AgentOperation::Permission { .. } => {}
            other => panic!("expected Permission, got {other:?}"),
        }
    }
}

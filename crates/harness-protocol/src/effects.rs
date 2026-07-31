//! Serializable effects produced by the deterministic agent core.

use serde::{Deserialize, Serialize};

use crate::backend::{BackendReference, ExecutionRequest};
use crate::commands::AgentResult;
use crate::events::{AgentEvent, AgentEventEnvelope};
use crate::ids::{AgentId, PermissionId, RunId, ToolCallId, ToolId};
use crate::tools::{AgentToolset, PermissionMode, ToolCall};
use crate::usage::AgentBudget;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    pub call: ToolCall,
    pub permission: PermissionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: PermissionId,
    pub tool_call: ToolCall,
    pub agent_id: AgentId,
}

/// A mutation to be applied to a session's durable store.
///
/// This is the minimal, self-contained payload shape that the deterministic
/// core emits via [`AgentEffect::Persist`]. It is defined directly in the
/// protocol crate so the core never depends on a concrete store
/// implementation. The `harness-session-store` crate is responsible for
/// converting these shapes into its own `DurableSessionEvent` /
/// `DurableSessionSnapshot` types before writing them (spec §59).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionMutation {
    /// Append a fully-qualified agent event to the durable session history.
    ///
    /// The envelope carries the event payload plus all routing/ordering
    /// metadata (`session_id`, `agent_id`, `session_sequence`, etc.)
    /// required to reconstruct a `DurableSessionEvent` on the store side.
    AppendEvent(AgentEventEnvelope),

    /// Persist a serialized session snapshot.
    ///
    /// The payload is an opaque, self-contained JSON value (typically the
    /// serialized form of the store's `DurableSessionSnapshot`) so the
    /// protocol does not depend on the store's concrete snapshot type.
    SaveSnapshot(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendPolicy {
    Inherit,
    Explicit(BackendReference),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolInheritance {
    InheritAll,
    Subset(Vec<ToolId>),
    Replace(AgentToolset),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspacePolicy {
    Inherit,
    ReadOnly,
    Snapshot,
    NewWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpawnMode {
    AwaitResult,
    Concurrent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnAgentSpec {
    pub role: Option<String>,
    pub backend: BackendPolicy,
    pub tools: ToolInheritance,
    pub workspace: WorkspacePolicy,
    pub budget: AgentBudget,
    pub mode: SpawnMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEffect {
    ExecuteBackend {
        request: ExecutionRequest,
    },
    ExecuteTool {
        request: ToolRequest,
    },
    /// Requests the spawning of a child agent.
    ///
    /// The runtime resolves this through the session's `AgentSupervisor`.
    SpawnAgent {
        spec: SpawnAgentSpec,
    },
    RequestPermission {
        request: PermissionRequest,
    },
    CancelBackend {
        run_id: RunId,
    },
    CancelTool {
        call_id: ToolCallId,
    },
    CancelChild {
        agent_id: AgentId,
    },
    Persist {
        mutation: SessionMutation,
    },
    Emit {
        event: AgentEvent,
    },
    FinishRun {
        result: AgentResult,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventVisibility;
    use crate::ids::{EventId, SessionId, Timestamp};

    #[test]
    fn inheritance_variants_are_serializable() {
        let value = ToolInheritance::Subset(vec![ToolId::new()]);
        let json = serde_json::to_string(&value).expect("serialize inheritance");
        let _: ToolInheritance = serde_json::from_str(&json).expect("deserialize inheritance");
    }

    #[test]
    fn policy_enums_round_trip() {
        for policy in [WorkspacePolicy::Inherit, WorkspacePolicy::ReadOnly] {
            let json = serde_json::to_string(&policy).expect("serialize policy");
            assert_eq!(
                serde_json::from_str::<WorkspacePolicy>(&json).unwrap(),
                policy
            );
        }
    }

    /// Helper: build a minimal fully-qualified event envelope for testing.
    fn envelope_for(event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: SessionId::new(),
            agent_id: crate::ids::AgentId::new(),
            parent_agent_id: None,
            run_id: None,
            agent_sequence: 0,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        }
    }

    #[test]
    fn session_mutation_append_event_round_trip() {
        let mutation = SessionMutation::AppendEvent(envelope_for(AgentEvent::RunStarted {
            run_id: RunId::new(),
        }));
        let json = serde_json::to_string(&mutation).expect("serialize append mutation");
        let deserialized: SessionMutation =
            serde_json::from_str(&json).expect("deserialize append mutation");
        match deserialized {
            SessionMutation::AppendEvent(envelope) => {
                assert!(matches!(envelope.event, AgentEvent::RunStarted { .. }));
            }
            other => panic!("expected AppendEvent, got {other:?}"),
        }
    }

    #[test]
    fn session_mutation_save_snapshot_round_trip() {
        let mutation = SessionMutation::SaveSnapshot(serde_json::json!({
            "session_id": "s1",
            "status": "Running",
        }));
        let json = serde_json::to_string(&mutation).expect("serialize snapshot mutation");
        let deserialized: SessionMutation =
            serde_json::from_str(&json).expect("deserialize snapshot mutation");
        match deserialized {
            SessionMutation::SaveSnapshot(payload) => {
                assert_eq!(payload["status"], "Running");
            }
            other => panic!("expected SaveSnapshot, got {other:?}"),
        }
    }
}

//! Serializable effects produced by the deterministic agent core.

use serde::{Deserialize, Serialize};

use crate::backend::{BackendReference, ExecutionRequest};
use crate::commands::AgentResult;
use crate::events::AgentEvent;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMutation {
    pub description: String,
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
}

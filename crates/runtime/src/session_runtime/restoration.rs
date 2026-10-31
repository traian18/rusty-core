//! Restore-time construction: rebuilds a session runtime from a durable snapshot's stored agent
//! states. The fresh-session counterpart lives in [`super::construction`].

use rusty_agent_core::agent::Agent;
use rusty_agent_core::agent_state::{AgentState, PendingToolCall};
use rusty_agent_core::behavior::{BehaviorRestoreError, BehaviorState};
use rusty_protocol::ids::{AgentId, SessionId};
use rusty_session_store::StoredAgentState;

use super::assembly::SessionHost;
use super::projection::{capabilities_from_value, usage_from_value};
use super::SessionRuntime;

/// Why a stored session cannot be rebuilt.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// A stored behavior no longer validates: restoring it would run the agent under different
    /// rules than the session ran with.
    #[error("agent {agent} behavior cannot be restored: {error}")]
    Behavior {
        agent: AgentId,
        error: BehaviorRestoreError,
    },
    #[error("a stored snapshot must contain exactly one root agent (parent_id = None)")]
    NoRootAgent,
}

impl SessionRuntime {
    /// Rebuilds a session runtime from a durable snapshot's stored agent states.
    ///
    /// This is the restore-time counterpart of [`start`](SessionRuntime::start): instead of
    /// creating a fresh root agent, it reconstructs every agent (root and descendants) from its
    /// stored projection and starts a runner for each. The root is the stored agent with no
    /// parent. The session starts [`Idle`](super::SessionStatus::Idle): a restored session accepts
    /// new commands rather than resuming a mid-flight run.
    ///
    /// When `host.session_store` is set, `Persist` effects keep flowing through the store after a
    /// restore, and the session's committer resumes after the last durable event.
    pub fn from_stored(
        host: SessionHost,
        stored_agents: Vec<StoredAgentState>,
    ) -> Result<Self, RestoreError> {
        let session_id = host.session_id;
        let mut agents = stored_agents
            .into_iter()
            .map(|stored| restored_agent(session_id, stored))
            .collect::<Result<Vec<_>, _>>()?;
        let root = agents
            .iter()
            .position(|agent| agent.parent_id.is_none())
            .map(|at| agents.swap_remove(at))
            .ok_or(RestoreError::NoRootAgent)?;
        Ok(Self::assemble(host, root, agents))
    }
}

/// One agent as it was stored.
fn restored_agent(session_id: SessionId, stored: StoredAgentState) -> Result<Agent, RestoreError> {
    let behavior = BehaviorState::from_stored(stored.behavior.as_ref()).map_err(|error| {
        RestoreError::Behavior {
            agent: stored.agent_id,
            error,
        }
    })?;
    Ok(Agent {
        id: stored.agent_id,
        session_id,
        parent_id: stored.parent_id,
        state: AgentState {
            status: stored.status,
            current_operation: stored.current_operation,
            system_prompt: stored.system_prompt,
            execution_params: stored.execution_params,
            messages: stored.messages,
            context: Default::default(),
            active_run: stored.active_run,
            // A restored session has no surviving backend task.
            backend_in_flight: false,
            queued_inputs: Default::default(),
            pending_tools: stored
                .pending_tools
                .into_iter()
                .map(|(call_id, pending)| {
                    let call = PendingToolCall {
                        call: pending.call,
                        started_at: pending.started_at,
                    };
                    (call_id, call)
                })
                .collect(),
            pending_permissions: stored.pending_permissions,
            children: stored.children,
            last_error: stored.last_error,
            transition_sequence: stored.transition_sequence,
            depth: stored.depth,
            behavior,
            pending_profile: None,
            consecutive_tool_timeouts: 0,
        },
        backend: stored.backend,
        capabilities: capabilities_from_value(&stored.capabilities),
        usage: usage_from_value(&stored.usage),
        budget: stored.budget,
    })
}

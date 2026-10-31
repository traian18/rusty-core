//! Reading a session: its snapshot, root agent's settings and live state.

use super::AgentLiveState;
use super::{SessionRuntime, SessionSnapshot, SessionStatus};
use crate::locks::MutexExt;
use std::sync::Arc;

use rusty_protocol::commands::AgentStatus;
use rusty_protocol::events::AgentOutcome;
use rusty_protocol::ids::AgentId;

impl SessionRuntime {
    /// Returns a [`SessionSnapshot`] of the current session state.
    ///
    /// This is a pure read — no stored copy, just a point-in-time projection
    /// of live state.
    pub fn state_snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock_or_recover();
        let live = self
            .live_state
            .lock_or_recover()
            .get(&state.root_agent_id)
            .cloned()
            .unwrap_or_default();
        let status = if state.status == SessionStatus::Failed {
            SessionStatus::Failed
        } else if self.cancellation.is_cancelled() {
            SessionStatus::Cancelled
        } else {
            match live.status {
                AgentStatus::Failed => SessionStatus::Failed,
                AgentStatus::Cancelled => SessionStatus::Cancelled,
                AgentStatus::Idle if live.last_outcome == Some(AgentOutcome::Success) => {
                    SessionStatus::Completed
                }
                AgentStatus::Idle => state.status,
                _ => SessionStatus::Running,
            }
        };
        let error = state
            .error
            .clone()
            .or_else(|| live.last_error.map(|error| error.message));
        SessionSnapshot {
            session_id: self.session_id,
            status,
            root_agent_id: state.root_agent_id,
            agent_count: state.agents.len(),
            error,
        }
    }

    /// The root agent's current execution parameters (model, max tokens,
    /// reasoning, ...), including any `ConfigureExecution` applied since
    /// construction.
    pub fn root_execution_params(&self) -> rusty_protocol::backend::ExecutionParams {
        let root = self.state.lock_or_recover().root_agent_id;
        self.projection
            .lock_or_recover()
            .get(&root)
            .map(|stored| stored.execution_params.clone())
            .unwrap_or_default()
    }

    /// The root agent's current behavior (profile and run counters), as last
    /// published by its runner.
    pub fn root_behavior(&self) -> rusty_agent_core::behavior::BehaviorState {
        let root = self.state.lock_or_recover().root_agent_id;
        let stored = self
            .projection
            .lock_or_recover()
            .get(&root)
            .and_then(|stored| stored.behavior.clone());
        rusty_agent_core::behavior::BehaviorState::from_stored(stored.as_ref()).unwrap_or_default()
    }

    /// Replace how this session's child agents get their behavior profile.
    pub fn set_child_behavior_resolver(
        &self,
        resolver: Arc<dyn rusty_agent_core::behavior::ChildBehaviorResolver>,
    ) {
        self.agent_supervisor.set_child_behavior_resolver(resolver);
    }

    /// The root agent's toolset: every tool it can see and its policy.
    pub fn root_toolset(&self) -> rusty_protocol::tools::AgentToolset {
        let state = self.state.lock_or_recover();
        state
            .agents
            .get(&state.root_agent_id)
            .map(|agent| agent.capabilities.tools.clone())
            .unwrap_or_else(|| rusty_protocol::tools::AgentToolset {
                tools: Default::default(),
            })
    }

    /// Returns the current live status/usage projection for `agent_id`.
    ///
    /// Returns [`AgentLiveState::default`] if the agent is unknown (e.g. it
    /// has not been spawned yet).
    pub fn agent_live_state(&self, agent_id: AgentId) -> AgentLiveState {
        self.live_state
            .lock_or_recover()
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
    }
}

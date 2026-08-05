//! Agent domain entity. All external work is represented as effects from Agent::apply.

use std::collections::HashMap;

use harness_protocol::backend::BackendBinding;
use harness_protocol::commands::AgentStatus;
use harness_protocol::ids::{AgentId, SessionId};
use harness_protocol::usage::AgentBudget;

use crate::agent_state::AgentState;
use crate::capabilities::AgentCapabilities;
use crate::context_state::AgentContextState;

#[derive(Debug, Clone, Default)]
pub struct UsageLedger {
    pub records: Vec<harness_protocol::usage::UsageRecord>,
    pub child_usage: HashMap<AgentId, crate::usage::AgentUsageSummary>,
}

impl UsageLedger {
    pub fn add_child_summary(
        &mut self,
        agent_id: AgentId,
        summary: crate::usage::AgentUsageSummary,
    ) {
        self.child_usage.insert(agent_id, summary);
    }
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub id: AgentId,
    pub session_id: SessionId,
    pub parent_id: Option<AgentId>,
    pub state: AgentState,
    pub backend: BackendBinding,
    pub capabilities: AgentCapabilities,
    pub usage: UsageLedger,
    pub budget: AgentBudget,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AgentId,
        session_id: SessionId,
        parent_id: Option<AgentId>,
        depth: u32,
        system_prompt: String,
        backend: BackendBinding,
        capabilities: AgentCapabilities,
        budget: AgentBudget,
    ) -> Self {
        Self {
            id,
            session_id,
            parent_id,
            state: AgentState {
                status: AgentStatus::Idle,
                current_operation: None,
                system_prompt,
                messages: Vec::new(),
                context: AgentContextState::default(),
                active_run: None,
                pending_tools: Default::default(),
                pending_permissions: Default::default(),
                children: Vec::new(),
                last_error: None,
                transition_sequence: 0,
                depth,
            },
            backend,
            capabilities,
            usage: UsageLedger::default(),
            budget,
        }
    }

    pub fn is_root(&self) -> bool {
        self.parent_id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harness_protocol::backend::{
        BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
    };
    use harness_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId, SessionId};
    use harness_protocol::tools::AgentToolset;

    use crate::capabilities::WorkspaceCapabilities;

    use super::*;

    fn agent(parent_id: Option<AgentId>) -> Agent {
        Agent::new(
            AgentId::new(),
            SessionId::new(),
            parent_id,
            0,
            "system".into(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset {
                    tools: HashMap::new(),
                },
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: WorkspaceCapabilities {
                    can_read: false,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        )
    }

    #[test]
    fn root_and_child_construction() {
        let root = agent(None);
        assert!(root.is_root());
        assert_eq!(root.state.status, AgentStatus::Idle);
        assert!(root.state.pending_permissions.is_empty());
        assert_eq!(root.state.context, AgentContextState::default());

        let child = agent(Some(root.id));
        assert!(!child.is_root());
        assert_eq!(child.parent_id, Some(root.id));
        assert_eq!(child.state.context.generation, 0);
    }

    #[test]
    fn depth_is_stored_on_state() {
        let agent = Agent::new(
            AgentId::new(),
            SessionId::new(),
            None,
            3,
            "system".into(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset {
                    tools: HashMap::new(),
                },
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: WorkspaceCapabilities {
                    can_read: false,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        );

        assert_eq!(agent.state.depth, 3);
    }
}

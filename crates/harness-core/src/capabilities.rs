use std::collections::HashMap;

use harness_protocol::backend::BackendCapabilities;
use harness_protocol::effects::ToolInheritance;
use harness_protocol::ids::ToolId;
use harness_protocol::tools::AgentToolset;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceCapabilities {
    pub can_read: bool,
    pub can_write: bool,
    pub can_search: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentCapabilities {
    pub tools: AgentToolset,
    pub can_spawn_agents: bool,
    pub max_child_depth: Option<u32>,
    pub workspace: WorkspaceCapabilities,
    pub backend: BackendCapabilities,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CapabilityError {
    #[error("Tool {0} not found in parent capabilities")]
    ToolNotFound(ToolId),

    #[error("Tool {0} is not delegatable")]
    NotDelegatable(ToolId),

    #[error("Tool {0} is not enabled in parent")]
    NotEnabled(ToolId),
}

impl AgentCapabilities {
    pub fn can_delegate(&self, tool_id: &ToolId) -> bool {
        self.tools
            .tools
            .get(tool_id)
            .map(|tc| tc.delegatable && tc.policy.enabled)
            .unwrap_or(false)
    }

    pub fn derive_child_capabilities(
        &self,
        inheritance: &ToolInheritance,
    ) -> Result<AgentToolset, CapabilityError> {
        let child_tools = match inheritance {
            ToolInheritance::InheritAll => self
                .tools
                .tools
                .iter()
                .filter(|(_, capability)| capability.delegatable && capability.policy.enabled)
                .map(|(id, capability)| (*id, capability.clone()))
                .collect(),

            ToolInheritance::Subset(ids) => {
                let mut tools = HashMap::new();
                for id in ids {
                    let capability = self
                        .tools
                        .tools
                        .get(id)
                        .ok_or(CapabilityError::ToolNotFound(*id))?;
                    if !capability.delegatable {
                        return Err(CapabilityError::NotDelegatable(*id));
                    }
                    if !capability.policy.enabled {
                        return Err(CapabilityError::NotEnabled(*id));
                    }
                    tools.insert(*id, capability.clone());
                }
                tools
            }

            ToolInheritance::Replace(toolset) => {
                for id in toolset.tools.keys() {
                    if !self.can_delegate(id) {
                        return Err(CapabilityError::NotDelegatable(*id));
                    }
                }
                toolset
                    .tools
                    .iter()
                    .map(|(id, requested)| {
                        let parent = &self.tools.tools[id];
                        let mut child = parent.clone();
                        child.policy.enabled &= requested.policy.enabled;
                        child.delegatable &= requested.delegatable;
                        use harness_protocol::tools::PermissionMode;
                        child.policy.permission =
                            match (&parent.policy.permission, &requested.policy.permission) {
                                (PermissionMode::Deny, _) | (_, PermissionMode::Deny) => {
                                    PermissionMode::Deny
                                }
                                (PermissionMode::Ask, _) | (_, PermissionMode::Ask) => {
                                    PermissionMode::Ask
                                }
                                _ => PermissionMode::Allow,
                            };
                        (*id, child)
                    })
                    .collect()
            }
        };

        Ok(AgentToolset { tools: child_tools })
    }

    pub fn derive_child_agent_capabilities(
        &self,
        inheritance: &ToolInheritance,
        workspace_override: Option<WorkspaceCapabilities>,
        backend_override: Option<BackendCapabilities>,
    ) -> Result<AgentCapabilities, CapabilityError> {
        let tools = self.derive_child_capabilities(inheritance)?;

        let max_child_depth = match self.max_child_depth {
            Some(0) => Some(0),
            Some(d) => Some(d - 1),
            None => None,
        };

        Ok(AgentCapabilities {
            tools,
            can_spawn_agents: self.can_spawn_agents && max_child_depth != Some(0),
            max_child_depth,
            workspace: workspace_override.unwrap_or_else(|| self.workspace.clone()),
            backend: backend_override.unwrap_or_else(|| self.backend.clone()),
        })
    }
}

#[cfg(test)]
mod tests;

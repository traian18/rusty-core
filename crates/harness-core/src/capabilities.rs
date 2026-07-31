//! Agent capabilities and non-escalation enforcement.
//!
//! This module defines how an agent's permissions are represented
//! ([`AgentCapabilities`], [`WorkspaceCapabilities`]) and how child
//! capabilities are derived from a parent while enforcing the
//! **non-escalation invariant** (spec §23):
//!
//! > `ChildTools ⊆ ParentDelegatableTools`
//!
//! The entry points are [`AgentCapabilities::can_delegate`],
//! [`AgentCapabilities::derive_child_capabilities`] and
//! [`AgentCapabilities::derive_child_agent_capabilities`].

use std::collections::HashMap;

use harness_protocol::backend::BackendCapabilities;
use harness_protocol::effects::ToolInheritance;
use harness_protocol::ids::ToolId;
use harness_protocol::tools::AgentToolset;

// ---------------------------------------------------------------------------
// WorkspaceCapabilities
// ---------------------------------------------------------------------------

/// Declares what workspace operations an agent is permitted to perform.
#[derive(Debug, Clone)]
pub struct WorkspaceCapabilities {
    /// Whether the agent can read files from the workspace.
    pub can_read: bool,
    /// Whether the agent can write files to the workspace.
    pub can_write: bool,
    /// Whether the agent can search the workspace.
    pub can_search: bool,
}

// ---------------------------------------------------------------------------
// AgentCapabilities
// ---------------------------------------------------------------------------

/// The full set of capabilities granted to an agent.
///
/// This includes the tools the agent may use, whether it can spawn children,
/// its workspace permissions, and the capabilities advertised by its backend.
///
/// The sub-agent non-escalation invariant (spec §23) is enforced by
/// [`derive_child_capabilities`](AgentCapabilities::derive_child_capabilities)
/// and [`derive_child_agent_capabilities`](AgentCapabilities::derive_child_agent_capabilities).
#[derive(Debug, Clone)]
pub struct AgentCapabilities {
    /// The tools this agent is allowed to use.
    pub tools: AgentToolset,
    /// Whether this agent may spawn sub-agents.
    pub can_spawn_agents: bool,
    /// Maximum allowed nesting depth for children (`None` = unlimited).
    pub max_child_depth: Option<u32>,
    /// Workspace access permissions.
    pub workspace: WorkspaceCapabilities,
    /// Capabilities of the backend this agent is bound to (cached for quick lookup).
    pub backend: BackendCapabilities,
}

// ---------------------------------------------------------------------------
// CapabilityError
// ---------------------------------------------------------------------------

/// Errors that can occur when deriving child capabilities.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CapabilityError {
    /// The requested tool does not exist in the parent's toolset.
    #[error("Tool {0} not found in parent capabilities")]
    ToolNotFound(ToolId),

    /// The requested tool exists but is not delegatable to children.
    #[error("Tool {0} is not delegatable")]
    NotDelegatable(ToolId),

    /// The requested tool is not enabled in the parent's toolset.
    #[error("Tool {0} is not enabled in parent")]
    NotEnabled(ToolId),
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

impl AgentCapabilities {
    /// Returns `true` if the given tool can be delegated to a child agent.
    ///
    /// A tool is delegatable **only if** it exists in the current agent's
    /// toolset **and** both `delegatable` and `policy.enabled` are `true`.
    ///
    /// If the tool is not found at all, returns `false`.
    pub fn can_delegate(&self, tool_id: &ToolId) -> bool {
        self.tools
            .tools
            .get(tool_id)
            .map(|tc| tc.delegatable && tc.policy.enabled)
            .unwrap_or(false)
    }

    /// Derives a child's toolset from a parent's capabilities using the
    /// specified inheritance strategy.
    ///
    /// This method enforces the **non-escalation invariant**:
    ///
    /// > A child must never automatically obtain capabilities that the parent
    /// > cannot delegate.
    ///
    /// # Invariants enforced per variant
    ///
    /// | Variant | Invariant |
    /// |---|---|
    /// | `InheritAll` | Child gets all parent tools — no extra check needed because the parent already owns them. |
    /// | `Subset(ids)` | Every tool in the list must exist in the parent, be delegatable, and be enabled. |
    /// | `Replace(toolset)` | Every tool in the replacement set must be delegatable from the parent. |
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError::ToolNotFound`] if a requested tool does not
    /// exist in the parent. Returns [`CapabilityError::NotDelegatable`] if a
    /// requested tool is not delegatable. Returns [`CapabilityError::NotEnabled`]
    /// if a requested tool is not enabled.
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
                toolset.tools.clone()
            }
        };

        Ok(AgentToolset { tools: child_tools })
    }

    /// Derives a child agent's full [`AgentCapabilities`] from a parent while
    /// enforcing the **non-escalation invariant** (spec §23).
    ///
    /// Unlike [`derive_child_capabilities`](Self::derive_child_capabilities),
    /// which only derives the child's *toolset*, this method derives the
    /// complete child capabilities:
    ///
    /// * **Tools** — derived via
    ///   [`derive_child_capabilities`](Self::derive_child_capabilities) using
    ///   the given [`ToolInheritance`] strategy.
    /// * **`max_child_depth`** — decremented from the parent
    ///   (`Some(d)` → `Some(d-1)`). A parent at `Some(0)` (unable to descend
    ///   further) yields a child that is also at `Some(0)`, so
    ///   `can_spawn_agents` stays `false` — never `None` (which means
    ///   *unlimited* and would escalate). `None` stays `None` (unlimited).
    /// * **`can_spawn_agents`** — a child may only spawn its own sub-agents if
    ///   the parent could spawn agents **and** the child still has positive
    ///   depth remaining (guarding against grandchildren).
    /// * **Workspace/Backend** — copied from the parent unless explicitly
    ///   overridden.
    ///
    /// # Errors
    ///
    /// Returns the same errors as
    /// [`derive_child_capabilities`](Self::derive_child_capabilities).
    pub fn derive_child_agent_capabilities(
        &self,
        inheritance: &ToolInheritance,
        workspace_override: Option<WorkspaceCapabilities>,
        backend_override: Option<BackendCapabilities>,
    ) -> Result<AgentCapabilities, CapabilityError> {
        let tools = self.derive_child_capabilities(inheritance)?;

        let max_child_depth = match self.max_child_depth {
            // A parent that cannot descend yields a child that cannot either.
            // Keeping `Some(0)` (instead of `None`) preserves the
            // non-escalation invariant: `None` means *unlimited* depth and
            // would silently re-grant spawning.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harness_protocol::backend::BackendCapabilities;
    use harness_protocol::effects::ToolInheritance;
    use harness_protocol::ids::ToolId;
    use harness_protocol::tools::{
        AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy,
    };

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Creates an `AgentCapabilities` with a single delegatable, enabled tool.
    fn parent_with_one_delegatable_tool() -> AgentCapabilities {
        let tool_id = ToolId::new();
        let mut tools = HashMap::new();
        tools.insert(
            tool_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: tool_id,
                    name: "fs.read".into(),
                    description: "Read files".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: true,
            },
        );

        AgentCapabilities {
            tools: AgentToolset { tools },
            can_spawn_agents: true,
            max_child_depth: Some(5),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities {
                tool_calls: true,
                ..Default::default()
            },
        }
    }

    /// Creates an `AgentCapabilities` with two tools:
    /// - `fs.read`    (delegatable, enabled)
    /// - `shell.exec` (not delegatable, enabled)
    fn parent_with_mixed_tools() -> (AgentCapabilities, ToolId, ToolId) {
        let fs_read_id = ToolId::new();
        let shell_exec_id = ToolId::new();
        let mut tools = HashMap::new();

        tools.insert(
            fs_read_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: fs_read_id,
                    name: "fs.read".into(),
                    description: "Read files".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: true,
            },
        );

        tools.insert(
            shell_exec_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: shell_exec_id,
                    name: "shell.exec".into(),
                    description: "Execute shell commands".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            },
        );

        let caps = AgentCapabilities {
            tools: AgentToolset { tools },
            can_spawn_agents: true,
            max_child_depth: Some(5),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities {
                tool_calls: true,
                ..Default::default()
            },
        };

        (caps, fs_read_id, shell_exec_id)
    }

    // -----------------------------------------------------------------------
    // can_delegate
    // -----------------------------------------------------------------------

    #[test]
    fn delegatable_enabled_tool_is_delegatable() {
        let caps = parent_with_one_delegatable_tool();
        let tool_id = *caps.tools.tools.keys().next().unwrap();
        assert!(caps.can_delegate(&tool_id));
    }

    #[test]
    fn non_delegatable_tool_is_not_delegatable() {
        let (caps, _fs_read_id, shell_exec_id) = parent_with_mixed_tools();
        assert!(!caps.can_delegate(&shell_exec_id));
    }

    #[test]
    fn non_existent_tool_is_not_delegatable() {
        let caps = parent_with_one_delegatable_tool();
        let unknown = ToolId::new();
        assert!(!caps.can_delegate(&unknown));
    }

    // -----------------------------------------------------------------------
    // derive_child_capabilities — InheritAll
    // -----------------------------------------------------------------------

    #[test]
    fn inherit_all_returns_only_delegatable_enabled_parent_tools() {
        let (caps, fs_read_id, shell_exec_id) = parent_with_mixed_tools();
        let result = caps.derive_child_capabilities(&ToolInheritance::InheritAll);
        assert!(result.is_ok());
        let child_set = result.unwrap();
        assert_eq!(child_set.tools.len(), 1);
        assert!(child_set.tools.contains_key(&fs_read_id));
        assert!(!child_set.tools.contains_key(&shell_exec_id));
    }

    // -----------------------------------------------------------------------
    // derive_child_capabilities — Subset
    // -----------------------------------------------------------------------

    #[test]
    fn subset_with_all_delegatable_succeeds() {
        let (caps, fs_read_id, _) = parent_with_mixed_tools();
        let result = caps.derive_child_capabilities(&ToolInheritance::Subset(vec![fs_read_id]));
        assert!(result.is_ok());
        let child_set = result.unwrap();
        assert_eq!(child_set.tools.len(), 1);
        assert!(child_set.tools.contains_key(&fs_read_id));
    }

    #[test]
    fn subset_with_non_delegatable_is_rejected() {
        let (caps, _fs_read_id, shell_exec_id) = parent_with_mixed_tools();
        let result = caps.derive_child_capabilities(&ToolInheritance::Subset(vec![shell_exec_id]));
        assert!(result.is_err());
        match result {
            Err(CapabilityError::NotDelegatable(id)) => assert_eq!(id, shell_exec_id),
            other => panic!("expected NotDelegatable, got {other:?}"),
        }
    }

    #[test]
    fn subset_with_non_existent_tool_is_rejected() {
        let caps = parent_with_one_delegatable_tool();
        let unknown = ToolId::new();
        let result = caps.derive_child_capabilities(&ToolInheritance::Subset(vec![unknown]));
        assert!(result.is_err());
        match result {
            Err(CapabilityError::ToolNotFound(id)) => assert_eq!(id, unknown),
            other => panic!("expected ToolNotFound, got {other:?}"),
        }
    }

    #[test]
    fn subset_rejects_not_enabled_tool() {
        let tool_id = ToolId::new();
        let mut tools = HashMap::new();
        tools.insert(
            tool_id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id: tool_id,
                    name: "disabled-tool".into(),
                    description: "A disabled tool".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Deny,
                    enabled: false,
                },
                delegatable: true,
            },
        );

        let caps = AgentCapabilities {
            tools: AgentToolset { tools },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: false,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        };

        let result = caps.derive_child_capabilities(&ToolInheritance::Subset(vec![tool_id]));
        assert!(result.is_err());
        match result {
            Err(CapabilityError::NotEnabled(id)) => assert_eq!(id, tool_id),
            other => panic!("expected NotEnabled, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // derive_child_capabilities — Replace
    // -----------------------------------------------------------------------

    #[test]
    fn replace_with_delegatable_tools_succeeds() {
        let caps = parent_with_one_delegatable_tool();
        let tool_id = *caps.tools.tools.keys().next().unwrap();

        // Build a replacement toolset with the same delegatable tool
        let mut replacement = HashMap::new();
        replacement.insert(tool_id, caps.tools.tools.get(&tool_id).unwrap().clone());
        let child_toolset = AgentToolset { tools: replacement };

        let result = caps.derive_child_capabilities(&ToolInheritance::Replace(child_toolset));
        assert!(result.is_ok());
        let child_set = result.unwrap();
        assert!(child_set.tools.contains_key(&tool_id));
    }

    #[test]
    fn replace_with_non_delegatable_tool_is_rejected() {
        let (caps, _fs_read_id, shell_exec_id) = parent_with_mixed_tools();

        // Build a replacement toolset with the non-delegatable tool
        let mut replacement = HashMap::new();
        replacement.insert(
            shell_exec_id,
            caps.tools.tools.get(&shell_exec_id).unwrap().clone(),
        );
        let child_toolset = AgentToolset { tools: replacement };

        let result = caps.derive_child_capabilities(&ToolInheritance::Replace(child_toolset));
        assert!(result.is_err());
        match result {
            Err(CapabilityError::NotDelegatable(id)) => assert_eq!(id, shell_exec_id),
            other => panic!("expected NotDelegatable, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // derive_child_agent_capabilities — non-escalation enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn child_cannot_spawn_agents_when_parent_cannot() {
        // (a) A parent that cannot spawn agents yields a child that cannot
        // spawn agents either (no escalation).
        let mut parent = parent_with_one_delegatable_tool();
        parent.can_spawn_agents = false;

        let child = parent
            .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
            .unwrap();

        assert!(!child.can_spawn_agents);
    }

    #[test]
    fn grandchild_spawning_is_blocked_at_depth_one() {
        // (b) A parent with max_child_depth = Some(1) yields a child with
        // max_child_depth = Some(0) and can_spawn_agents = false (grandchildren
        // blocked).
        let mut parent = parent_with_one_delegatable_tool();
        parent.max_child_depth = Some(1);

        let child = parent
            .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
            .unwrap();

        assert_eq!(child.max_child_depth, Some(0));
        assert!(!child.can_spawn_agents);
    }

    #[test]
    fn zero_depth_parent_yields_zero_depth_child() {
        // A parent at max_child_depth = Some(0) yields a child that is also
        // at Some(0) and cannot spawn — NOT None (unlimited), which would
        // escalate.
        let mut parent = parent_with_one_delegatable_tool();
        parent.max_child_depth = Some(0);

        let child = parent
            .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
            .unwrap();

        assert_eq!(child.max_child_depth, Some(0));
        assert!(!child.can_spawn_agents);
    }

    #[test]
    fn subset_child_toolset_only_contains_delegatable_tools() {
        // (c) Subset([fs.read]) from a parent with fs.read (delegatable) and
        // shell.exec (not delegatable) yields a child toolset with only fs.read.
        let (parent, fs_read_id, shell_exec_id) = parent_with_mixed_tools();

        let child = parent
            .derive_child_agent_capabilities(&ToolInheritance::Subset(vec![fs_read_id]), None, None)
            .unwrap();

        assert_eq!(child.tools.tools.len(), 1);
        assert!(child.tools.tools.contains_key(&fs_read_id));
        assert!(!child.tools.tools.contains_key(&shell_exec_id));
    }
}

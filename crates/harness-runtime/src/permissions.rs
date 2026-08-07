//! Permission policy for tool execution at the runtime boundary.
//!
//! The [`PermissionPolicy`] provides a defense-in-depth layer that re-evaluates
//! whether a tool call should be allowed, require approval, or be denied,
//! consulting the agent's [`AgentCapabilities`] and the underlying
//! [`AgentToolset`](harness_protocol::tools::AgentToolset) directly — even
//! though the core state machine has already filtered the tool request
//! during [`Agent::apply`](harness_core::agent::Agent::apply).
//!
//! This runtime check ensures that **even if** a bug, misconfiguration, or
//! malformed effect causes the core to emit an `ExecuteTool` for a tool that
//! is not enabled or is denied by policy, the runner will reject the call
//! before dispatching it to the executor.

use harness_core::capabilities::AgentCapabilities;
use harness_protocol::tools::PermissionMode;

/// Evaluates whether a tool call should be allowed, require approval, or be denied.
///
/// Stateless and cloneable — a single instance is typically used across the
/// entire runner lifetime.
#[derive(Debug, Clone, Default)]
pub struct PermissionPolicy;

/// Result of a permission evaluation.
///
/// The runner maps this outcome to either dispatching the tool, surfacing a
/// permission-required signal, or failing the tool call.
#[derive(Debug)]
pub enum PermissionOutcome {
    /// No restriction — the executor may run the tool.
    Allow,
    /// The caller must obtain user approval before executing.
    RequiresApproval(String),
    /// The tool is blocked by policy.
    Denied(String),
}

impl PermissionPolicy {
    /// Evaluate whether a tool with the given *name* (e.g. `"fs.read"`) is
    /// permitted for the agent described by `capabilities`.
    ///
    /// # Lookup logic
    ///
    /// 1. The agent's toolset (`capabilities.tools`) is searched for a tool
    ///    capability whose `descriptor.name` matches `tool_name`.
    /// 2. If no match is found → [`Denied`](PermissionOutcome::Denied).
    /// 3. If the tool is found but `policy.enabled` is `false` →
    ///    [`Denied`](PermissionOutcome::Denied).
    /// 4. Otherwise the [`PermissionMode`] is inspected:
    ///    - `Allow` → [`Allow`](PermissionOutcome::Allow)
    ///    - `Ask` → [`RequiresApproval`](PermissionOutcome::RequiresApproval)
    ///    - `Deny` → [`Denied`](PermissionOutcome::Denied)
    pub fn evaluate(&self, capabilities: &AgentCapabilities, tool_name: &str) -> PermissionOutcome {
        // Find the tool capability by descriptor name (the logical tool
        // identifier used in tool calls, e.g. "fs.read").
        let tool_cap = capabilities
            .tools
            .tools
            .values()
            .find(|tc| tc.descriptor.name == tool_name);

        match tool_cap {
            None => PermissionOutcome::Denied(format!(
                "Tool '{}' is not present in the agent's toolset",
                tool_name
            )),
            Some(tc) => {
                if !tc.policy.enabled {
                    return PermissionOutcome::Denied(format!(
                        "Tool '{}' is not enabled in the agent's toolset",
                        tool_name
                    ));
                }
                match tc.policy.permission {
                    PermissionMode::Allow => PermissionOutcome::Allow,
                    PermissionMode::Ask => PermissionOutcome::RequiresApproval(format!(
                        "Tool '{}' requires user approval",
                        tool_name
                    )),
                    PermissionMode::Deny => PermissionOutcome::Denied(format!(
                        "Tool '{}' is denied by policy",
                        tool_name
                    )),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
    use harness_protocol::backend::BackendCapabilities;
    use harness_protocol::ids::ToolId;
    use harness_protocol::tools::{AgentToolset, ToolCapability, ToolDescriptor};

    use super::*;

    /// Helper to build an `AgentCapabilities` with one tool of the given
    /// permission mode and enabled state.
    fn capabilities_with_tool(
        tool_name: &str,
        permission: PermissionMode,
        enabled: bool,
    ) -> AgentCapabilities {
        let id = ToolId::new();
        let mut tools = HashMap::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: tool_name.to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                },
                policy: harness_protocol::tools::ToolPolicy {
                    permission,
                    enabled,
                },
                delegatable: false,
            },
        );
        AgentCapabilities {
            tools: AgentToolset { tools },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: false,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        }
    }

    #[test]
    fn allow_enabled_tool_returns_allow() {
        let caps = capabilities_with_tool("fs.read", PermissionMode::Allow, true);
        let outcome = PermissionPolicy.evaluate(&caps, "fs.read");
        assert!(matches!(outcome, PermissionOutcome::Allow));
    }

    #[test]
    fn ask_tool_returns_requires_approval() {
        let caps = capabilities_with_tool("bash.exec", PermissionMode::Ask, true);
        let outcome = PermissionPolicy.evaluate(&caps, "bash.exec");
        assert!(matches!(outcome, PermissionOutcome::RequiresApproval(_)));
    }

    #[test]
    fn deny_tool_is_denied() {
        let caps = capabilities_with_tool("dangerous.write", PermissionMode::Deny, true);
        let outcome = PermissionPolicy.evaluate(&caps, "dangerous.write");
        assert!(matches!(outcome, PermissionOutcome::Denied(_)));
    }

    #[test]
    fn disabled_tool_is_denied_even_when_allow() {
        let caps = capabilities_with_tool("disabled.tool", PermissionMode::Allow, false);
        let outcome = PermissionPolicy.evaluate(&caps, "disabled.tool");
        assert!(matches!(outcome, PermissionOutcome::Denied(_)));
    }

    #[test]
    fn unknown_tool_is_denied() {
        let caps = capabilities_with_tool("known.tool", PermissionMode::Allow, true);
        let outcome = PermissionPolicy.evaluate(&caps, "unknown.tool");
        assert!(matches!(outcome, PermissionOutcome::Denied(_)));
    }

    #[test]
    fn empty_toolset_denies_everything() {
        let caps = AgentCapabilities {
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
        };
        let outcome = PermissionPolicy.evaluate(&caps, "anything");
        assert!(matches!(outcome, PermissionOutcome::Denied(_)));
    }
}

//! Application skill grants intersected with harness-owned mode restrictions.

use harness_protocol::tools::{AgentToolset, ExecutionMode, ExecutionPolicy, PermissionMode};

/// How much of an MCP server a session may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerAccess {
    Denied,
    /// Only the tools the server marks `readOnlyHint: true`.
    ReadOnly,
    Full,
}

pub fn mcp_server_access(policy: &ExecutionPolicy, server: &str) -> McpServerAccess {
    // MCP servers reach the network (and stdio ones spawn processes), so they
    // need an explicit per-server grant plus network permission.
    if !policy.allowed_mcp_servers.iter().any(|name| name == server)
        || !enabled(policy, "web_search")
    {
        return McpServerAccess::Denied;
    }
    // Tools that may write only run where the session may really write;
    // plan, virtual, and write-less sessions get the read-only ones.
    if policy.mode == ExecutionMode::Execute && enabled(policy, "write_file") {
        McpServerAccess::Full
    } else {
        McpServerAccess::ReadOnly
    }
}

fn enabled(policy: &ExecutionPolicy, name: &str) -> bool {
    policy.enabled_tools.iter().any(|tool| tool == name)
}

pub fn allows_tool(policy: &ExecutionPolicy, name: &str) -> bool {
    let permission = match name {
        "report_progress" | "ask_user_question" => return true,
        "write_plan" => return policy.mode == ExecutionMode::Plan,
        "read_file" | "fs.read" | "open_document" => "read_file",
        "write_file" | "fs.edit" => {
            if policy.mode == ExecutionMode::Plan {
                return false;
            }
            "write_file"
        }
        "list_files" => "list_files",
        "search_codebase" | "workspace.search" => "search_codebase",
        "web_search" | "web_fetch" => "web_search",
        "run_command" | "shell.exec" => {
            // The current command executor has host filesystem/network access.
            if policy.mode != ExecutionMode::Execute
                || !enabled(policy, "write_file")
                || !enabled(policy, "web_search")
            {
                return false;
            }
            "run_command"
        }
        _ => return false,
    };
    enabled(policy, permission)
}

/// Apply a ceiling to the existing policy; never change Ask/Deny into Allow.
/// MCP IDs come from discovery of explicitly allowed servers, not model input
/// or matching an ambiguous `mcp.<server>` name prefix.
pub fn restrict_toolset(policy: &ExecutionPolicy, tools: &mut AgentToolset, mcp_tools: &[String]) {
    for capability in tools.tools.values_mut() {
        let name = &capability.descriptor.name;
        if !allows_tool(policy, name) && !mcp_tools.contains(name) {
            capability.policy.permission = PermissionMode::Deny;
            capability.policy.enabled = false;
            capability.delegatable = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_requires_exact_server_grants_and_network_and_limits_non_writing_sessions_to_read_only() {
        let mut policy = ExecutionPolicy {
            mode: ExecutionMode::Execute,
            enabled_tools: vec!["write_file".into(), "web_search".into()],
            allowed_mcp_servers: vec!["docs".into()],
        };
        assert_eq!(mcp_server_access(&policy, "docs"), McpServerAccess::Full);
        for name in ["docs-other", "docs.child", "other"] {
            assert_eq!(mcp_server_access(&policy, name), McpServerAccess::Denied);
        }
        policy.mode = ExecutionMode::Plan;
        assert_eq!(
            mcp_server_access(&policy, "docs"),
            McpServerAccess::ReadOnly
        );
        policy.mode = ExecutionMode::Virtual;
        assert_eq!(
            mcp_server_access(&policy, "docs"),
            McpServerAccess::ReadOnly
        );
        policy.mode = ExecutionMode::Execute;
        policy.enabled_tools.retain(|name| name != "write_file");
        assert_eq!(
            mcp_server_access(&policy, "docs"),
            McpServerAccess::ReadOnly
        );
        policy.enabled_tools.retain(|name| name != "web_search");
        assert_eq!(mcp_server_access(&policy, "docs"), McpServerAccess::Denied);
    }

    #[test]
    fn unrestricted_commands_cannot_bypass_file_or_network_restrictions() {
        let mut policy = ExecutionPolicy {
            mode: ExecutionMode::Execute,
            enabled_tools: vec!["run_command".into()],
            allowed_mcp_servers: vec![],
        };
        assert!(!allows_tool(&policy, "run_command"));
        policy.enabled_tools.push("write_file".into());
        assert!(!allows_tool(&policy, "run_command"));
        policy.enabled_tools.push("web_search".into());
        assert!(allows_tool(&policy, "run_command"));
        policy.mode = ExecutionMode::Plan;
        assert!(!allows_tool(&policy, "run_command"));
    }
}

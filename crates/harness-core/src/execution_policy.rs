//! Application skill grants intersected with harness-owned mode restrictions.

use harness_protocol::tools::{AgentToolset, ExecutionMode, ExecutionPolicy, PermissionMode};

pub fn allows_mcp_server(policy: &ExecutionPolicy, server: &str) -> bool {
    // MCP servers may execute arbitrary processes, writes and network requests.
    // Until executors can enforce narrower effects, do not admit them into a
    // read-only, virtual, or offline skill session.
    policy.mode == ExecutionMode::Execute
        && enabled(policy, "write_file")
        && enabled(policy, "web_search")
        && policy.allowed_mcp_servers.iter().any(|name| name == server)
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
    fn mcp_requires_exact_server_grants_and_a_mode_that_can_execute_it() {
        let mut policy = ExecutionPolicy {
            mode: ExecutionMode::Execute,
            enabled_tools: vec!["write_file".into(), "web_search".into()],
            allowed_mcp_servers: vec!["docs".into()],
        };
        assert!(allows_mcp_server(&policy, "docs"));
        for name in ["docs-other", "docs.child", "other"] {
            assert!(!allows_mcp_server(&policy, name));
        }
        policy.mode = ExecutionMode::Plan;
        assert!(!allows_mcp_server(&policy, "docs"));
        policy.mode = ExecutionMode::Virtual;
        assert!(!allows_mcp_server(&policy, "docs"));
        policy.mode = ExecutionMode::Execute;
        policy.enabled_tools.retain(|name| name != "web_search");
        assert!(!allows_mcp_server(&policy, "docs"));
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

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
        // `decide` is advisory: the IDE asks a decision model and returns its pick.
        "report_progress" | "ask_user_question" | "decide" => return true,
        "write_plan" => return policy.mode == ExecutionMode::Plan,
        // `project_info` is the IDE's project-detection tool: it only reads
        // manifests, so it shares the `read_file` grant (in plan mode too).
        "read_file" | "fs.read" | "open_document" | "project_info" => "read_file",
        // `edit_file` is the IDE's targeted patch tool: same grant as `write_file`.
        "write_file" | "edit_file" | "fs.edit" => {
            if policy.mode == ExecutionMode::Plan {
                return false;
            }
            "write_file"
        }
        "list_files" => "list_files",
        "search_codebase" | "workspace.search" => "search_codebase",
        // web_extract fetches a page like web_fetch, so it needs the same network grant.
        "web_search" | "web_fetch" | "web_extract" => "web_search",
        // `run_check` (the IDE's project-check tool) and `install_dependencies`
        // run commands through the same executor, so they need everything
        // `run_command` needs.
        "run_command" | "shell.exec" | "run_check" | "install_dependencies" => {
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
mod tests;

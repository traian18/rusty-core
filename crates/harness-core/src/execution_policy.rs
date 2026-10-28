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
    if is_unconditionally_allowed(name) {
        return true;
    }
    if name == "write_plan" {
        return policy.mode == ExecutionMode::Plan;
    }
    if is_write_tool(name) && policy.mode == ExecutionMode::Plan {
        return false;
    }
    if name == "save_workflow" && policy.mode != ExecutionMode::Execute {
        return false;
    }
    if is_command_tool(name) {
        return allows_command_execution(policy);
    }
    tool_permission(name).is_some_and(|permission| enabled(policy, permission))
}

fn is_unconditionally_allowed(name: &str) -> bool {
    matches!(
        name,
        "report_progress"
            | "ask_user_question"
            | "decide"
            | "update_todos"
            | "job_output"
            | "spec_tasks_check"
    ) || crate::tool_alias::ALWAYS_ADMITTED.contains(&name)
}

fn is_write_tool(name: &str) -> bool {
    matches!(name, "write_file" | "edit_file" | "fs.edit")
}

fn is_command_tool(name: &str) -> bool {
    matches!(
        name,
        "run_command" | "shell.exec" | "run_check" | "install_dependencies"
    )
}

fn allows_command_execution(policy: &ExecutionPolicy) -> bool {
    policy.mode == ExecutionMode::Execute
        && enabled(policy, "write_file")
        && enabled(policy, "web_search")
        && enabled(policy, "run_command")
}

fn tool_permission(name: &str) -> Option<&'static str> {
    match name {
        "read_file" | "fs.read" | "open_document" | "project_info" | "request_research"
        | "workflow_catalog" | "repo_context" | "repo_wiki" | "spec_context" => Some("read_file"),
        "write_file" | "edit_file" | "fs.edit" => Some("write_file"),
        "save_workflow" => Some("save_workflow"),
        "list_files" => Some("list_files"),
        "search_codebase" | "workspace.search" => Some("search_codebase"),
        "web_search" | "web_fetch" | "web_extract" => Some("web_search"),
        _ => None,
    }
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

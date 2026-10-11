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

#[test]
fn web_extract_needs_the_same_network_grant_as_web_fetch() {
    let mut policy = ExecutionPolicy {
        mode: ExecutionMode::Plan,
        enabled_tools: vec!["web_search".into()],
        allowed_mcp_servers: vec![],
    };
    assert!(allows_tool(&policy, "web_extract"));
    policy.enabled_tools.clear();
    assert!(!allows_tool(&policy, "web_extract"));
    assert!(!allows_tool(&policy, "web_fetch"));
}

#[test]
fn edit_file_follows_the_write_file_grant_and_never_runs_in_plan_mode() {
    let mut policy = ExecutionPolicy {
        mode: ExecutionMode::Execute,
        enabled_tools: vec!["write_file".into()],
        allowed_mcp_servers: vec![],
    };
    assert!(allows_tool(&policy, "edit_file"));
    policy.mode = ExecutionMode::Virtual;
    assert!(allows_tool(&policy, "edit_file"));
    policy.mode = ExecutionMode::Plan;
    assert!(!allows_tool(&policy, "edit_file"));
    policy.mode = ExecutionMode::Execute;
    policy.enabled_tools = vec!["read_file".into()];
    assert!(
        !allows_tool(&policy, "edit_file"),
        "no write grant, no edit_file"
    );
}

#[test]
fn run_check_needs_everything_run_command_needs() {
    let mut policy = ExecutionPolicy {
        mode: ExecutionMode::Execute,
        enabled_tools: vec!["run_command".into()],
        allowed_mcp_servers: vec![],
    };
    assert!(
        !allows_tool(&policy, "run_check"),
        "commands also need file and network grants"
    );
    policy.enabled_tools.push("write_file".into());
    assert!(!allows_tool(&policy, "run_check"));
    policy.enabled_tools.push("web_search".into());
    assert!(allows_tool(&policy, "run_check"));
    assert!(allows_tool(&policy, "install_dependencies"));
    policy.mode = ExecutionMode::Plan;
    assert!(!allows_tool(&policy, "run_check"));
    assert!(!allows_tool(&policy, "install_dependencies"));
    policy.mode = ExecutionMode::Virtual;
    assert!(!allows_tool(&policy, "run_check"));
    policy.mode = ExecutionMode::Execute;
    policy.enabled_tools.retain(|name| name != "run_command");
    assert!(
        !allows_tool(&policy, "run_check"),
        "no run_command grant, no run_check"
    );
}

#[test]
fn project_info_follows_the_read_file_grant_in_every_mode() {
    let mut policy = ExecutionPolicy {
        mode: ExecutionMode::Execute,
        enabled_tools: vec!["read_file".into()],
        allowed_mcp_servers: vec![],
    };
    for mode in [
        ExecutionMode::Execute,
        ExecutionMode::Plan,
        ExecutionMode::Virtual,
    ] {
        policy.mode = mode;
        assert!(allows_tool(&policy, "project_info"));
    }
    policy.enabled_tools = vec!["write_file".into()];
    assert!(
        !allows_tool(&policy, "project_info"),
        "writing does not grant reading"
    );
    policy.enabled_tools.clear();
    assert!(!allows_tool(&policy, "project_info"));
}

#[test]
fn decide_is_allowed_without_any_grant_in_every_mode() {
    for mode in [
        ExecutionMode::Execute,
        ExecutionMode::Plan,
        ExecutionMode::Virtual,
    ] {
        let policy = ExecutionPolicy {
            mode,
            enabled_tools: vec![],
            allowed_mcp_servers: vec![],
        };
        assert!(allows_tool(&policy, "decide"));
    }
}

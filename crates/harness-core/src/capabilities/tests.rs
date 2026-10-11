use std::collections::HashMap;

use harness_protocol::backend::BackendCapabilities;
use harness_protocol::effects::ToolInheritance;
use harness_protocol::ids::ToolId;
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy,
};

use super::*;

#[test]
fn replacement_cannot_change_tool_identity_or_remove_parent_approval() {
    let mut parent = parent_with_one_delegatable_tool();
    let id = *parent.tools.tools.keys().next().unwrap();
    parent.tools.tools.get_mut(&id).unwrap().policy.permission = PermissionMode::Ask;
    let mut replacement = parent.tools.clone();
    let tool = replacement.tools.get_mut(&id).unwrap();
    tool.descriptor.name = "shell.exec".into();
    tool.policy.permission = PermissionMode::Allow;
    let child = parent
        .derive_child_capabilities(&ToolInheritance::Replace(replacement))
        .unwrap();
    assert_eq!(child.tools[&id].descriptor.name, "fs.read");
    assert!(matches!(
        child.tools[&id].policy.permission,
        PermissionMode::Ask
    ));
}

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

#[test]
fn replace_with_delegatable_tools_succeeds() {
    let caps = parent_with_one_delegatable_tool();
    let tool_id = *caps.tools.tools.keys().next().unwrap();

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

#[test]
fn child_cannot_spawn_agents_when_parent_cannot() {
    let mut parent = parent_with_one_delegatable_tool();
    parent.can_spawn_agents = false;

    let child = parent
        .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
        .unwrap();

    assert!(!child.can_spawn_agents);
}

#[test]
fn grandchild_spawning_is_blocked_at_depth_one() {
    let mut parent = parent_with_one_delegatable_tool();
    parent.max_child_depth = Some(1);
    let child = parent
        .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
        .expect("derive child");
    assert_eq!(child.max_child_depth, Some(0));
    assert!(!child.can_spawn_agents);
}

#[test]
fn zero_depth_parent_yields_zero_depth_child() {
    let mut parent = parent_with_one_delegatable_tool();
    parent.max_child_depth = Some(0);
    let child = parent
        .derive_child_agent_capabilities(&ToolInheritance::InheritAll, None, None)
        .expect("derive child");
    assert_eq!(child.max_child_depth, Some(0));
    assert!(!child.can_spawn_agents);
}

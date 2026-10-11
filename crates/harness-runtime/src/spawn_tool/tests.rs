use std::collections::HashMap;

use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use harness_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId, SessionId};
use harness_protocol::tools::{AgentToolset, PermissionMode, ToolCapability, ToolPolicy};

use super::*;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};

fn tool_capability(name: &str, delegatable: bool, enabled: bool) -> (ToolId, ToolCapability) {
    let id = ToolId::new();
    (
        id,
        ToolCapability {
            descriptor: ToolDescriptor {
                id,
                name: name.to_string(),
                description: "test tool".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            policy: ToolPolicy {
                permission: PermissionMode::Allow,
                enabled,
            },
            delegatable,
        },
    )
}

fn parent_agent(tools: Vec<(ToolId, ToolCapability)>, budget: AgentBudget) -> Agent {
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        "parent system prompt".into(),
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
                tools: tools.into_iter().collect::<HashMap<_, _>>(),
            },
            can_spawn_agents: true,
            max_child_depth: Some(3),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: false,
                can_search: true,
            },
            backend: BackendCapabilities::default(),
        },
        budget,
    )
}

#[test]
fn missing_task_is_rejected() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let result = build_spawn_spec(&serde_json::json!({}), &parent);
    assert!(result.is_err());
}

#[test]
fn blank_task_is_rejected() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let result = build_spawn_spec(&serde_json::json!({"task": "   "}), &parent);
    assert!(result.is_err());
}

#[test]
fn defaults_are_least_privilege_read_only() {
    let read_tool = tool_capability("fs.read", true, true);
    let edit_tool = tool_capability("fs.edit", true, true);
    let parent = parent_agent(vec![read_tool.clone(), edit_tool], AgentBudget::default());

    let (spec, warnings) =
        build_spawn_spec(&serde_json::json!({"task": "look around"}), &parent).unwrap();

    assert!(warnings.is_empty());
    assert_eq!(spec.workspace, WorkspacePolicy::ReadOnly);
    assert_eq!(spec.mode, SpawnMode::AwaitResult);
    match spec.tools {
        ToolInheritance::Subset(ids) => {
            assert_eq!(
                ids,
                vec![read_tool.0],
                "fs.edit must not be in the default grant"
            );
        }
        other => panic!("expected Subset, got {other:?}"),
    }
}

#[test]
fn explicit_tool_request_is_honored_when_delegatable() {
    let edit_tool = tool_capability("fs.edit", true, true);
    let parent = parent_agent(vec![edit_tool.clone()], AgentBudget::default());

    let (spec, warnings) = build_spawn_spec(
        &serde_json::json!({"task": "fix the bug", "tools": ["fs.edit"]}),
        &parent,
    )
    .unwrap();

    assert!(warnings.is_empty());
    match spec.tools {
        ToolInheritance::Subset(ids) => assert_eq!(ids, vec![edit_tool.0]),
        other => panic!("expected Subset, got {other:?}"),
    }
}

/// M5: an explicit `model` argument becomes an `execution_params.model`
/// override on the built spec, applied to the child's `AgentState` at
/// construction by `AgentSupervisor::spawn_child` — see
/// `SpawnAgentSpec::execution_params`'s doc comment for why this goes
/// through `ExecutionParams` rather than `BackendPolicy::Explicit`.
#[test]
fn model_override_becomes_an_execution_params_override() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let (spec, _warnings) = build_spawn_spec(
        &serde_json::json!({"task": "summarize this file", "model": "claude-haiku-4-5"}),
        &parent,
    )
    .unwrap();

    assert_eq!(
        spec.execution_params.model.as_deref(),
        Some("claude-haiku-4-5")
    );
    assert!(
        matches!(spec.backend, BackendPolicy::Inherit),
        "a model override must not switch the child to a different backend policy"
    );
}

/// Omitting `model` entirely must leave `execution_params` at its
/// default (no override) — the pre-existing behavior for every spawn
/// that doesn't ask for a model override.
#[test]
fn no_model_argument_leaves_execution_params_unset() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let (spec, _warnings) =
        build_spawn_spec(&serde_json::json!({"task": "summarize this file"}), &parent).unwrap();

    assert_eq!(spec.execution_params.model, None);
}

#[test]
fn non_delegatable_tool_is_never_granted_even_if_requested() {
    let locked_tool = tool_capability("shell.exec", false, true);
    let parent = parent_agent(vec![locked_tool], AgentBudget::default());

    let (spec, warnings) = build_spawn_spec(
        &serde_json::json!({"task": "run a command", "tools": ["shell.exec"]}),
        &parent,
    )
    .unwrap();

    assert_eq!(
        warnings.len(),
        1,
        "the ungranted request must be reported, not silent"
    );
    match spec.tools {
        ToolInheritance::Subset(ids) => {
            assert!(ids.is_empty(), "non-delegatable tool must never be granted")
        }
        other => panic!("expected Subset, got {other:?}"),
    }
}

#[test]
fn requesting_a_tool_the_parent_does_not_have_is_never_granted() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let (spec, warnings) = build_spawn_spec(
        &serde_json::json!({"task": "do something", "tools": ["fs.edit"]}),
        &parent,
    )
    .unwrap();
    assert_eq!(warnings.len(), 1);
    match spec.tools {
        ToolInheritance::Subset(ids) => assert!(ids.is_empty()),
        other => panic!("expected Subset, got {other:?}"),
    }
}

#[test]
fn unset_budget_defaults_to_the_parents_own_ceiling_not_unlimited() {
    let parent_budget = AgentBudget {
        max_requests: Some(10),
        max_cost_usd: Some(rust_decimal_macros::dec!(5.00)),
        ..Default::default()
    };
    let parent = parent_agent(vec![], parent_budget.clone());

    let (spec, _) = build_spawn_spec(&serde_json::json!({"task": "go"}), &parent).unwrap();

    assert_eq!(spec.budget.max_requests, parent_budget.max_requests);
    assert_eq!(spec.budget.max_cost_usd, parent_budget.max_cost_usd);
}

#[test]
fn explicit_tighter_budget_override_is_preserved() {
    let parent_budget = AgentBudget {
        max_requests: Some(10),
        ..Default::default()
    };
    let parent = parent_agent(vec![], parent_budget);

    let (spec, _) = build_spawn_spec(
        &serde_json::json!({"task": "go", "budget": {"max_requests": 3}}),
        &parent,
    )
    .unwrap();

    assert_eq!(spec.budget.max_requests, Some(3));
}

#[test]
fn invalid_workspace_value_is_rejected() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let result = build_spawn_spec(
        &serde_json::json!({"task": "go", "workspace": "not-a-real-policy"}),
        &parent,
    );
    assert!(result.is_err());
}

#[test]
fn concurrent_mode_parses_correctly() {
    let parent = parent_agent(vec![], AgentBudget::default());
    let (spec, _) = build_spawn_spec(
        &serde_json::json!({"task": "go", "mode": "concurrent"}),
        &parent,
    )
    .unwrap();
    assert_eq!(spec.mode, SpawnMode::Concurrent);
}

#[test]
fn descriptor_has_the_stable_tool_name_and_requires_task() {
    let descriptor = agent_spawn_tool_descriptor(ToolId::new());
    assert_eq!(descriptor.name, AGENT_SPAWN_TOOL_NAME);
    assert_eq!(
        descriptor.input_schema["required"],
        serde_json::json!(["task"])
    );
}

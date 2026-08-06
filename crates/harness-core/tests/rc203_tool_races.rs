//! RC-203 cancellation certification for tool and permission boundaries.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
};
use harness_protocol::commands::{AgentCommand, PermissionDecision, UserInput};
use harness_protocol::effects::AgentEffect;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, SessionId, ToolCallId,
};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy,
};
use harness_protocol::usage::AgentBudget;

fn agent(permission: PermissionMode) -> Agent {
    let descriptor_id = harness_protocol::ids::ToolId::new();
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        "system".into(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "tool-race-backend".into(),
                description: "deterministic test backend".into(),
                capabilities: BackendCapabilities {
                    tool_calls: true,
                    ..Default::default()
                },
            },
        },
        AgentCapabilities {
            tools: AgentToolset {
                tools: HashMap::from([(
                    descriptor_id,
                    ToolCapability {
                        descriptor: ToolDescriptor {
                            id: descriptor_id,
                            name: "test-tool".into(),
                            description: "test tool".into(),
                            input_schema: serde_json::json!({"type": "object"}),
                        },
                        policy: ToolPolicy {
                            permission,
                            enabled: true,
                        },
                        delegatable: false,
                    },
                )]),
            },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: false,
                can_search: true,
            },
            backend: BackendCapabilities {
                tool_calls: true,
                ..Default::default()
            },
        },
        AgentBudget::default(),
    )
}

fn start(agent: &mut Agent) -> harness_protocol::ids::RunId {
    agent
        .apply(AgentCommand::StartRun {
            input: UserInput {
                text: "tool race".into(),
                attachments: vec![],
            },
        })
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request.run_id),
            _ => None,
        })
        .expect("backend request")
}

fn request_tool(agent: &mut Agent, run_id: harness_protocol::ids::RunId) -> ToolCallId {
    let call_id = ToolCallId::new();
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: harness_protocol::ids::RequestId::new(),
            call: ToolCall {
                id: call_id,
                name: "test-tool".into(),
                arguments: serde_json::json!({}),
            },
        },
    });
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::ExecuteTool { .. } | AgentEffect::RequestPermission { .. }
    )));
    call_id
}

#[test]
fn cancellation_in_tool_execution_ignores_late_tool_result() {
    let mut agent = agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let call_id = request_tool(&mut agent, run_id);

    let effects = agent.apply(AgentCommand::Cancel);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::CancelTool { call_id: cancelled } if *cancelled == call_id
    )));
    assert!(agent.state.pending_tools.is_empty());

    let late = agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: harness_protocol::tools::ToolResult {
            call_id,
            output: serde_json::json!({"late": true}),
            is_error: false,
        },
    });
    assert!(late.is_empty());
    assert!(agent.state.messages.iter().all(|message| {
        !message.content.iter().any(|content| matches!(
            content,
            harness_protocol::messages::ContentBlock::ToolResult { .. }
        ))
    }));
}

#[test]
fn cancellation_in_permission_wait_ignores_late_resolution() {
    let mut agent = agent(PermissionMode::Ask);
    let run_id = start(&mut agent);
    let call_id = request_tool(&mut agent, run_id);

    let permission_id = agent
        .state
        .pending_permissions
        .keys()
        .next()
        .copied()
        .expect("permission request");
    agent.apply(AgentCommand::Cancel);

    assert!(agent.state.pending_tools.is_empty());
    assert!(agent.state.pending_permissions.is_empty());
    assert!(agent
        .apply(AgentCommand::PermissionResolved {
            id: permission_id,
            decision: PermissionDecision::Approved,
        })
        .is_empty());
    assert!(!agent.state.pending_tools.contains_key(&call_id));
}

//! Synchronous, I/O-free tests for the Phase 1 deterministic core transition surface.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, CapabilityError, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionResult,
};
use harness_protocol::commands::{
    AgentCommand, AgentError, AgentResult, AgentStatus, PermissionDecision, UserInput,
};
use harness_protocol::effects::{AgentEffect, ToolInheritance};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, SessionId, ToolCallId, ToolId,
};
use harness_protocol::messages::{ContentBlock, MessageRole};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolError, ToolPolicy,
    ToolResult,
};
use harness_protocol::usage::{AgentBudget, AgentUsageSummary, Cost, ModelUsage, UsageValue};

fn create_agent(permission: PermissionMode) -> Agent {
    let tool_id = ToolId::new();
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        "system".into(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "fake".into(),
                description: "fake backend".into(),
                capabilities: BackendCapabilities {
                    tool_calls: true,
                    ..Default::default()
                },
            },
        },
        AgentCapabilities {
            tools: AgentToolset {
                tools: HashMap::from([(
                    tool_id,
                    ToolCapability {
                        descriptor: ToolDescriptor {
                            id: tool_id,
                            name: "test-tool".into(),
                            description: "test".into(),
                            input_schema: serde_json::json!({"type": "object"}),
                        },
                        policy: ToolPolicy {
                            permission,
                            enabled: true,
                        },
                        delegatable: true,
                    },
                )]),
            },
            can_spawn_agents: true,
            max_child_depth: Some(3),
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
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "hello".into(),
            attachments: vec![],
        },
    });
    assert!(matches!(effects[0], AgentEffect::Emit { event: AgentEvent::StateChanged { .. } }));
    assert!(matches!(effects[1], AgentEffect::ExecuteBackend { .. }));
    assert!(matches!(effects[2], AgentEffect::Emit { event: AgentEvent::RunStarted { .. } }));
    assert_eq!(agent.state.messages[0].role, MessageRole::User);
    agent.state.active_run.expect("active run")
}

fn tool_call(call_id: ToolCallId) -> ToolCall {
    ToolCall {
        id: call_id,
        name: "test-tool".into(),
        arguments: serde_json::json!({"value": 1}),
    }
}

fn completed_result() -> ExecutionResult {
    ExecutionResult {
        request_id: RequestId::new(),
        usage: ModelUsage {
            input_tokens: UsageValue::new(Some(2)),
            output_tokens: UsageValue::new(Some(1)),
            total_tokens: UsageValue::new(Some(3)),
            ..Default::default()
        },
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    }
}

#[test]
fn tool_call_emits_execute_effect() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    assert!(effects.iter().any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));
    assert!(agent.state.pending_tools.contains_key(&call_id));
}

#[test]
fn permission_ask_blocks_until_resolved() {
    let mut agent = create_agent(PermissionMode::Ask);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    let permission_id = effects.iter().find_map(|effect| match effect {
        AgentEffect::RequestPermission { request } => Some(request.id),
        _ => None,
    }).expect("permission request");
    assert_eq!(agent.state.status, AgentStatus::WaitingForPermission);
    assert!(!effects.iter().any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));

    let approved = agent.apply(AgentCommand::PermissionResolved {
        id: permission_id,
        decision: PermissionDecision::Approved,
    });
    assert_eq!(agent.state.status, AgentStatus::Executing);
    assert!(approved.iter().any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));
}

#[test]
fn scripted_vertical_slice() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();

    let requested = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    assert!(matches!(requested.as_slice(), [
        AgentEffect::Emit { event: AgentEvent::ToolCallRequested { .. } },
        AgentEffect::ExecuteTool { .. }
    ]));

    let completed = agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output: serde_json::json!({"ok": true}),
            is_error: false,
        },
    });
    assert!(matches!(completed.as_slice(), [
        AgentEffect::Emit { event: AgentEvent::ToolCallCompleted { .. } },
        AgentEffect::ExecuteBackend { .. }
    ]));
    assert!(agent.state.pending_tools.is_empty());
    assert!(matches!(agent.state.messages.last().unwrap().content[0], ContentBlock::ToolResult { .. }));

    let finished = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    assert!(matches!(finished.as_slice(), [
        AgentEffect::Emit { event: AgentEvent::StateChanged { .. } },
        AgentEffect::Emit { event: AgentEvent::Completed { outcome: AgentOutcome::Success } },
        AgentEffect::FinishRun { .. }
    ]));
    assert_eq!(agent.state.status, AgentStatus::Idle);
    assert!(agent.state.active_run.is_none());
}

#[test]
fn cancel_stops_further_effects() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let effects = agent.apply(AgentCommand::Cancel);
    assert!(effects.iter().any(|effect| matches!(effect, AgentEffect::CancelBackend { run_id: id } if *id == run_id)));
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert!(agent.state.active_run.is_none());
    assert!(agent.apply(AgentCommand::Pause).is_empty());
}

#[test]
fn tool_failure_records_result_and_continues() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    let effects = agent.apply(AgentCommand::ToolFailed {
        call_id,
        error: ToolError::Timeout,
    });
    assert!(matches!(effects.as_slice(), [
        AgentEffect::Emit { event: AgentEvent::ToolCallCompleted { .. } },
        AgentEffect::ExecuteBackend { .. }
    ]));
}

#[test]
fn child_commands_do_not_crash_parent_state() {
    let mut agent = create_agent(PermissionMode::Allow);
    let child = AgentId::new();
    agent.state.children.push(child);
    let effects = agent.apply(AgentCommand::ChildCompleted {
        agent_id: child,
        result: AgentResult {
            summary: "done".into(),
            usage: AgentUsageSummary::default(),
        },
    });
    assert!(agent.state.children.is_empty());
    assert!(matches!(effects[0], AgentEffect::Emit { event: AgentEvent::ChildAgentCompleted { outcome: AgentOutcome::Success, .. } }));

    let failed_child = AgentId::new();
    agent.state.children.push(failed_child);
    agent.apply(AgentCommand::ChildFailed {
        agent_id: failed_child,
        error: AgentError {
            message: "failed".into(),
            code: "CHILD".into(),
            details: None,
        },
    });
    assert!(agent.state.children.is_empty());
    assert_eq!(agent.state.status, AgentStatus::Idle);
}

#[test]
fn pause_and_resume_cover_both_commands() {
    let mut agent = create_agent(PermissionMode::Allow);
    start(&mut agent);
    assert!(!agent.apply(AgentCommand::Pause).is_empty());
    assert_eq!(agent.state.status, AgentStatus::Paused);
    let effects = agent.apply(AgentCommand::Resume);
    assert_eq!(agent.state.status, AgentStatus::WaitingForBackend);
    assert!(effects.iter().any(|effect| matches!(effect, AgentEffect::ExecuteBackend { .. })));
}

#[test]
fn backend_delta_usage_and_error_events_are_covered() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::TextDelta {
            request_id: RequestId::new(),
            delta: "hello".into(),
        },
    });
    assert_eq!(agent.state.status, AgentStatus::Streaming);
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ReasoningDelta {
            request_id: RequestId::new(),
            delta: "why".into(),
        },
    });
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::UsageUpdate {
            request_id: RequestId::new(),
            usage: ModelUsage::default(),
        },
    });
    assert_eq!(agent.usage.records.len(), 1);
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Error {
            request_id: RequestId::new(),
            error: harness_protocol::backend::ExecutionError::Timeout,
        },
    });
    assert_eq!(agent.state.status, AgentStatus::Failed);
}

#[test]
fn non_delegatable_tool_rejected_for_child() {
    let allowed = ToolId::new();
    let blocked = ToolId::new();
    let capability = |id, name: &str, delegatable| ToolCapability {
        descriptor: ToolDescriptor {
            id,
            name: name.into(),
            description: name.into(),
            input_schema: serde_json::json!({}),
        },
        policy: ToolPolicy {
            permission: PermissionMode::Allow,
            enabled: true,
        },
        delegatable,
    };
    let capabilities = AgentCapabilities {
        tools: AgentToolset {
            tools: HashMap::from([
                (allowed, capability(allowed, "fs.read", true)),
                (blocked, capability(blocked, "shell.exec", false)),
            ]),
        },
        can_spawn_agents: true,
        max_child_depth: Some(2),
        workspace: WorkspaceCapabilities {
            can_read: true,
            can_write: false,
            can_search: false,
        },
        backend: BackendCapabilities::default(),
    };
    assert!(capabilities.derive_child_capabilities(&ToolInheritance::Subset(vec![allowed])).is_ok());
    assert!(matches!(
        capabilities.derive_child_capabilities(&ToolInheritance::Subset(vec![allowed, blocked])),
        Err(CapabilityError::NotDelegatable(id)) if id == blocked
    ));
}

#[test]
fn unknown_usage_stays_none_through_a_full_run() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: ExecutionResult {
                request_id: RequestId::new(),
                usage: ModelUsage {
                    input_tokens: UsageValue::new(None),
                    output_tokens: UsageValue::new(Some(0)),
                    ..Default::default()
                },
                cost: Cost::default(),
                finish_reason: "done".into(),
            },
        },
    });
    let usage = agent.usage.self_usage();
    assert!(usage.input_tokens.is_unknown());
    assert_eq!(usage.output_tokens.value(), Some(0));
}

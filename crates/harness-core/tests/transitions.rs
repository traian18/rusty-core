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
    assert!(matches!(
        effects[0],
        AgentEffect::Emit {
            event: AgentEvent::StateChanged { .. }
        }
    ));
    assert!(matches!(effects[1], AgentEffect::ExecuteBackend { .. }));
    assert!(matches!(
        effects[2],
        AgentEffect::Emit {
            event: AgentEvent::RunStarted { .. }
        }
    ));
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
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));
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
    let permission_id = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::RequestPermission { request } => Some(request.id),
            _ => None,
        })
        .expect("permission request");
    assert_eq!(agent.state.status, AgentStatus::WaitingForPermission);
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));

    let approved = agent.apply(AgentCommand::PermissionResolved {
        id: permission_id,
        decision: PermissionDecision::Approved,
    });
    assert_eq!(agent.state.status, AgentStatus::Executing);
    assert!(approved
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. })));
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
    assert!(matches!(
        requested.as_slice(),
        [
            AgentEffect::Emit {
                event: AgentEvent::ToolCallRequested { .. }
            },
            AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    to: AgentStatus::Executing,
                    ..
                }
            },
            AgentEffect::ExecuteTool { .. }
        ]
    ));

    let completed = agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output: serde_json::json!({"ok": true}),
            is_error: false,
        },
    });
    assert!(matches!(
        completed.as_slice(),
        [
            AgentEffect::Emit {
                event: AgentEvent::ToolCallCompleted { .. }
            },
            AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::Executing,
                    to: AgentStatus::WaitingForBackend
                }
            },
            AgentEffect::ExecuteBackend { .. }
        ]
    ));
    assert!(agent.state.pending_tools.is_empty());
    assert!(matches!(
        agent.state.messages.last().unwrap().content[0],
        ContentBlock::ToolResult { .. }
    ));

    let finished = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    assert!(matches!(
        finished.as_slice(),
        [
            AgentEffect::Emit {
                event: AgentEvent::StateChanged { .. }
            },
            AgentEffect::Emit {
                event: AgentEvent::Completed {
                    outcome: AgentOutcome::Success
                }
            },
            AgentEffect::FinishRun { .. }
        ]
    ));
    assert_eq!(agent.state.status, AgentStatus::Idle);
    assert!(agent.state.active_run.is_none());
}

#[test]
fn tool_use_completion_keeps_the_run_active() {
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

    let mut result = completed_result();
    result.finish_reason = "tool_use".into();
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result,
        },
    });

    assert!(effects.is_empty());
    assert_eq!(agent.state.active_run, Some(run_id));
    assert_ne!(agent.state.status, AgentStatus::Idle);
    assert_eq!(agent.usage.records.len(), 1);
}

#[test]
fn cancel_stops_further_effects() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);
    let effects = agent.apply(AgentCommand::Cancel);
    assert!(effects.iter().any(
        |effect| matches!(effect, AgentEffect::CancelBackend { run_id: id } if *id == run_id)
    ));
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert!(agent.state.active_run.is_none());
    assert!(agent.apply(AgentCommand::Pause).is_empty());
}

#[test]
fn cancelling_active_run_preserves_admitted_follow_ups() {
    let mut agent = create_agent(PermissionMode::Allow);
    let cancelled_run = start(&mut agent);
    agent.apply(AgentCommand::FollowUp {
        input: UserInput {
            text: "continue after cancellation".into(),
            attachments: vec![],
        },
    });

    let effects = agent.apply(AgentCommand::Cancel);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::CancelBackend { run_id } if *run_id == cancelled_run
    )));
    assert_eq!(agent.state.queued_inputs.len(), 1);

    let next_effects = agent.apply(AgentCommand::StartNextQueuedRun);
    assert!(next_effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::ExecuteBackend { request }
            if matches!(
                &request.messages.last().expect("queued message").content[0],
                ContentBlock::Text { text } if text == "continue after cancellation"
            )
    )));
    assert!(agent.state.active_run.is_some());
    assert_eq!(agent.state.queued_inputs.len(), 0);
}

#[test]
fn follow_ups_are_admitted_fifo_and_start_after_completion() {
    let mut agent = create_agent(PermissionMode::Allow);
    let first_run = start(&mut agent);

    assert!(agent
        .apply(AgentCommand::FollowUp {
            input: UserInput {
                text: "second".into(),
                attachments: vec![]
            },
        })
        .is_empty());
    assert!(agent
        .apply(AgentCommand::FollowUp {
            input: UserInput {
                text: "third".into(),
                attachments: vec![]
            },
        })
        .is_empty());
    assert_eq!(agent.state.queued_inputs.len(), 2);
    assert_eq!(agent.state.messages.len(), 1);

    let first_completion = agent.apply(AgentCommand::BackendEvent {
        run_id: first_run,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    assert!(matches!(
        first_completion.as_slice(),
        [
            AgentEffect::Emit {
                event: AgentEvent::StateChanged { .. }
            },
            AgentEffect::Emit {
                event: AgentEvent::Completed {
                    outcome: AgentOutcome::Success
                }
            },
            AgentEffect::FinishRun { .. }
        ]
    ));
    let second_start = agent.apply(AgentCommand::StartNextQueuedRun);
    assert!(second_start.iter().any(|effect| matches!(
        effect,
        AgentEffect::ExecuteBackend { request } if request.messages.last().is_some_and(|message| matches!(&message.content[0], ContentBlock::Text { text } if text == "second"))
    )));
    let second_run = agent.state.active_run.expect("second run starts");
    assert_ne!(first_run, second_run);
    assert_eq!(agent.state.queued_inputs.len(), 1);

    agent.apply(AgentCommand::BackendEvent {
        run_id: second_run,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    let third_start = agent.apply(AgentCommand::StartNextQueuedRun);
    assert!(third_start
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteBackend { .. })));
    assert!(agent.state.active_run.is_some());
    assert_eq!(agent.state.queued_inputs.len(), 0);
    assert!(
        matches!(&agent.state.messages.last().unwrap().content[0], ContentBlock::Text { text } if text == "third")
    );
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
    assert!(matches!(
        effects.as_slice(),
        [
            AgentEffect::Emit {
                event: AgentEvent::ToolCallCompleted { .. }
            },
            AgentEffect::Emit {
                event: AgentEvent::StateChanged {
                    from: AgentStatus::Executing,
                    to: AgentStatus::WaitingForBackend
                }
            },
            AgentEffect::ExecuteBackend { .. }
        ]
    ));
}

#[test]
fn cancellation_wins_over_a_late_permission_approval() {
    let mut agent = create_agent(PermissionMode::Ask);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();
    let requested = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    let permission_id = requested
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::RequestPermission { request } => Some(request.id),
            _ => None,
        })
        .expect("permission request");

    let cancelled = agent.apply(AgentCommand::Cancel);
    assert!(cancelled.iter().any(|effect| matches!(
        effect,
        AgentEffect::CancelTool { call_id: id } if *id == call_id
    )));
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert!(agent.state.pending_permissions.is_empty());
    assert!(agent.state.pending_tools.is_empty());

    let late = agent.apply(AgentCommand::PermissionResolved {
        id: permission_id,
        decision: PermissionDecision::Approved,
    });
    assert!(
        late.is_empty(),
        "late approval must not execute the cancelled tool"
    );
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
}

#[test]
fn a_permission_decision_is_applied_at_most_once() {
    let mut agent = create_agent(PermissionMode::Ask);
    let run_id = start(&mut agent);
    let call_id = ToolCallId::new();
    let requested = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: tool_call(call_id),
        },
    });
    let permission_id = requested
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::RequestPermission { request } => Some(request.id),
            _ => None,
        })
        .expect("permission request");

    let first = agent.apply(AgentCommand::PermissionResolved {
        id: permission_id,
        decision: PermissionDecision::Approved,
    });
    assert_eq!(
        first
            .iter()
            .filter(|effect| matches!(effect, AgentEffect::ExecuteTool { .. }))
            .count(),
        1
    );

    let duplicate = agent.apply(AgentCommand::PermissionResolved {
        id: permission_id,
        decision: PermissionDecision::Denied,
    });
    assert!(duplicate.is_empty(), "resolved permission IDs are consumed");
    assert_eq!(agent.state.status, AgentStatus::Executing);
    assert!(agent.state.pending_tools.contains_key(&call_id));
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
    assert!(matches!(
        effects[0],
        AgentEffect::Emit {
            event: AgentEvent::ChildAgentCompleted {
                outcome: AgentOutcome::Success,
                ..
            }
        }
    ));

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
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteBackend { .. })));
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
    assert_eq!(
        agent.usage.records.len(),
        0,
        "cumulative usage snapshots must not become request ledger records"
    );
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
    assert!(capabilities
        .derive_child_capabilities(&ToolInheritance::Subset(vec![allowed]))
        .is_ok());
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

/// M3: a runaway or adversarial backend stream must not grow an assistant
/// message's assembled text unbounded — `AgentState.messages` is held for
/// the run's (and, once persisted, the session's) entire lifetime.
#[test]
fn assistant_text_accumulation_is_capped_across_many_deltas() {
    let mut agent = create_agent(PermissionMode::Allow);
    let run_id = start(&mut agent);

    // Feed far more text than the cap across many separate deltas, the way
    // a real streaming backend would deliver it incrementally.
    let chunk = "x".repeat(64 * 1024);
    for _ in 0..100 {
        agent.apply(AgentCommand::BackendEvent {
            run_id,
            event: ExecutionEvent::TextDelta {
                request_id: RequestId::new(),
                delta: chunk.clone(),
            },
        });
    }

    let assistant_text: String = agent
        .state
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    // 100 * 64KiB = ~6.25MiB fed in, well past the 4MiB cap.
    const MAX_ASSISTANT_TEXT_BYTES: usize = 4 * 1024 * 1024;
    const MARKER: &str = "\n... (truncated, exceeds assistant message size limit)";
    assert!(
        assistant_text.len() <= MAX_ASSISTANT_TEXT_BYTES + MARKER.len(),
        "assembled assistant text must not exceed the cap plus marker, got {} bytes",
        assistant_text.len()
    );
    assert!(
        assistant_text.ends_with(MARKER),
        "truncated assistant text must carry an explicit marker"
    );
}

// ---------------------------------------------------------------------------
// M4: ConfigureExecution / ExecutionParams
// ---------------------------------------------------------------------------

#[test]
fn configure_execution_is_a_pure_state_mutation_with_no_effects() {
    let mut agent = create_agent(PermissionMode::Allow);
    let effects = agent.apply(AgentCommand::ConfigureExecution {
        params: harness_protocol::backend::ExecutionParams {
            model: Some("claude-opus-4-20250514".to_string()),
            max_tokens: Some(8192),
            ..Default::default()
        },
    });
    assert!(
        effects.is_empty(),
        "ConfigureExecution must not produce effects"
    );
    assert_eq!(
        agent.state.execution_params.model.as_deref(),
        Some("claude-opus-4-20250514")
    );
    assert_eq!(agent.state.execution_params.max_tokens, Some(8192));
}

#[test]
fn configured_execution_params_reach_the_next_runs_execution_request() {
    let mut agent = create_agent(PermissionMode::Allow);
    agent.apply(AgentCommand::ConfigureExecution {
        params: harness_protocol::backend::ExecutionParams {
            model: Some("gpt-4.1".to_string()),
            temperature: Some(0.3),
            extended_thinking: Some(true),
            ..Default::default()
        },
    });

    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "hello".into(),
            attachments: vec![],
        },
    });
    let request = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request),
            _ => None,
        })
        .expect("StartRun must produce an ExecuteBackend effect");

    assert_eq!(request.params.model.as_deref(), Some("gpt-4.1"));
    assert_eq!(request.params.temperature, Some(0.3));
    // The standalone `extended_thinking` bool mirrors `params.extended_thinking`
    // for source compatibility with existing readers.
    assert!(request.extended_thinking);
}

#[test]
fn configure_execution_is_a_partial_update_that_preserves_unset_fields() {
    let mut agent = create_agent(PermissionMode::Allow);
    agent.apply(AgentCommand::ConfigureExecution {
        params: harness_protocol::backend::ExecutionParams {
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(4096),
            ..Default::default()
        },
    });
    // Second update only changes temperature; model/max_tokens must survive.
    agent.apply(AgentCommand::ConfigureExecution {
        params: harness_protocol::backend::ExecutionParams {
            temperature: Some(0.9),
            ..Default::default()
        },
    });

    assert_eq!(
        agent.state.execution_params.model.as_deref(),
        Some("claude-sonnet-4-20250514")
    );
    assert_eq!(agent.state.execution_params.max_tokens, Some(4096));
    assert_eq!(agent.state.execution_params.temperature, Some(0.9));
}

#[test]
fn default_execution_params_produce_the_prior_hardcoded_behavior() {
    // Before M4, extended_thinking was hardcoded false and no model/params
    // were ever forwarded. An agent that never receives ConfigureExecution
    // must reproduce exactly that behavior.
    let mut agent = create_agent(PermissionMode::Allow);
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "hello".into(),
            attachments: vec![],
        },
    });
    let request = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request),
            _ => None,
        })
        .expect("StartRun must produce an ExecuteBackend effect");

    assert!(!request.extended_thinking);
    assert_eq!(
        request.params,
        harness_protocol::backend::ExecutionParams::default()
    );
}

// ---------------------------------------------------------------------------
// M4: attachments
// ---------------------------------------------------------------------------

use harness_protocol::commands::Attachment;

#[test]
fn image_attachment_becomes_a_real_content_block_not_silently_dropped() {
    let mut agent = create_agent(PermissionMode::Allow);
    agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "what's in this image?".into(),
            attachments: vec![Attachment {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3, 4],
            }],
        },
    });

    let message = &agent.state.messages[0];
    assert_eq!(
        message.content.len(),
        2,
        "text block + image block, nothing dropped"
    );
    assert!(
        matches!(&message.content[0], ContentBlock::Text { text } if text == "what's in this image?")
    );
    match &message.content[1] {
        ContentBlock::Image { mime_type, data } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(data, &vec![1, 2, 3, 4]);
        }
        other => panic!("expected an Image block, got {other:?}"),
    }
}

#[test]
fn non_image_attachment_becomes_a_visible_text_note_not_silently_dropped() {
    let mut agent = create_agent(PermissionMode::Allow);
    agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "see attached".into(),
            attachments: vec![Attachment {
                mime_type: "application/pdf".to_string(),
                data: vec![0; 100],
            }],
        },
    });

    let message = &agent.state.messages[0];
    assert_eq!(message.content.len(), 2);
    assert!(matches!(
        &message.content[1],
        ContentBlock::Text { text } if text.contains("application/pdf") && text.contains("100 bytes")
    ));
}

#[test]
fn oversized_attachment_fails_the_run_instead_of_silently_truncating_image_bytes() {
    let mut agent = create_agent(PermissionMode::Allow);
    const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "too big".into(),
            attachments: vec![Attachment {
                mime_type: "image/png".to_string(),
                data: vec![0; MAX_ATTACHMENT_BYTES + 1],
            }],
        },
    });

    assert_eq!(agent.state.status, AgentStatus::Failed);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::Emit { event: AgentEvent::Failed { error } } if error.code == "ATTACHMENT_TOO_LARGE"
    )));
}

// ---------------------------------------------------------------------------
// `UsageLedger::runs` (M4/M5 total_runs counter)
// ---------------------------------------------------------------------------

/// A successfully completed run increments `usage.runs`, and that count is
/// reflected in the very `FinishRun` effect the completing run itself emits
/// — not just visible to some later run.
#[test]
fn a_successful_run_increments_the_runs_counter_in_its_own_finish_effect() {
    let mut agent = create_agent(PermissionMode::Allow);
    assert_eq!(agent.usage.runs, 0);
    let run_id = start(&mut agent);

    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });

    assert_eq!(agent.usage.runs, 1);
    let finish_usage = effects.iter().find_map(|effect| match effect {
        AgentEffect::FinishRun { result } => Some(result.usage.clone()),
        _ => None,
    });
    assert_eq!(
        finish_usage
            .expect("a successful run must emit FinishRun")
            .inclusive_usage
            .total_runs,
        1,
        "the completing run's own FinishRun effect must already report the incremented count"
    );
}

/// A run that fails validation before any backend request is ever made
/// still goes through `fail()` and so is still counted — see `UsageLedger::runs`'s
/// doc comment for why that's the accepted, documented definition.
#[test]
fn a_run_rejected_by_validation_is_still_counted() {
    let mut agent = create_agent(PermissionMode::Allow);
    const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
    agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "too big".into(),
            attachments: vec![Attachment {
                mime_type: "image/png".to_string(),
                data: vec![0; MAX_ATTACHMENT_BYTES + 1],
            }],
        },
    });
    assert_eq!(agent.usage.runs, 1);
}

/// Cancellation does not emit `FinishRun` (see `Agent::cancel`'s doc
/// comment) and so does not increment `usage.runs` — a cancelled run is not
/// silently counted as a completed one.
#[test]
fn a_cancelled_run_does_not_increment_the_runs_counter() {
    let mut agent = create_agent(PermissionMode::Allow);
    start(&mut agent);
    let effects = agent.apply(AgentCommand::Cancel);
    assert_eq!(agent.usage.runs, 0);
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::FinishRun { .. })));
}

/// Two runs in sequence each increment the counter exactly once — it is a
/// running total across the agent's lifetime, not a per-run flag.
#[test]
fn multiple_runs_accumulate_the_counter() {
    let mut agent = create_agent(PermissionMode::Allow);
    let first_run = start(&mut agent);
    agent.apply(AgentCommand::BackendEvent {
        run_id: first_run,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    assert_eq!(agent.usage.runs, 1);

    let second_run = start(&mut agent);
    agent.apply(AgentCommand::BackendEvent {
        run_id: second_run,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed_result(),
        },
    });
    assert_eq!(agent.usage.runs, 2);
}

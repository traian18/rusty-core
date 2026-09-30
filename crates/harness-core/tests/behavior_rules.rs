//! Behavior rules driven through the deterministic agent state machine:
//! what the model sees, what runs, what is refused, and what is reported.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionRequest, ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentStatus, PermissionDecision, UserInput};
use harness_protocol::effects::AgentEffect;
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, RunId, SessionId, ToolCallId,
    ToolId,
};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy, ToolResult,
};
use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};
use serde_json::{json, Value};

fn agent(rules: Value) -> Agent {
    let tools = [
        ("fs.read", PermissionMode::Allow),
        ("fs.edit", PermissionMode::Allow),
        ("smart_fetch", PermissionMode::Allow),
        ("infer_decision", PermissionMode::Allow),
        ("shell.exec", PermissionMode::Ask),
        ("rm", PermissionMode::Deny),
    ]
    .into_iter()
    .map(|(name, permission)| {
        let id = ToolId::new();
        (
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.into(),
                    description: name.into(),
                    input_schema: json!({"type": "object"}),
                },
                policy: ToolPolicy {
                    permission,
                    enabled: true,
                },
                delegatable: true,
            },
        )
    })
    .collect::<HashMap<_, _>>();
    let mut agent = Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        String::new(),
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
            tools: AgentToolset { tools },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: true,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    );
    agent.apply(AgentCommand::SetBehaviorProfile {
        library: Vec::new(),
        allow_commands: false,
        profile: json!({
            "schema_version": 1, "id": "rules", "revision": 1, "name": "Rules",
            "rules": rules
        }),
    });
    agent
}

fn start(agent: &mut Agent) -> (RunId, Vec<AgentEffect>) {
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "go".into(),
            attachments: vec![],
        },
    });
    let run_id = request(&effects).expect("first request").run_id;
    (run_id, effects)
}

fn request(effects: &[AgentEffect]) -> Option<&ExecutionRequest> {
    effects.iter().find_map(|effect| match effect {
        AgentEffect::ExecuteBackend { request } => Some(request),
        _ => None,
    })
}

fn tool(
    agent: &mut Agent,
    run_id: RunId,
    name: &str,
    arguments: Value,
) -> (ToolCallId, Vec<AgentEffect>) {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: name.into(),
        arguments,
    };
    let id = call.id;
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call,
        },
    });
    (id, effects)
}

fn end_turn(agent: &mut Agent, run_id: RunId, finish_reason: &str) -> Vec<AgentEffect> {
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id,
            result: ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: finish_reason.into(),
            },
        },
    })
}

fn complete(
    agent: &mut Agent,
    call_id: ToolCallId,
    output: Value,
    is_error: bool,
) -> Vec<AgentEffect> {
    agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output,
            is_error,
        },
    })
}

fn executes(effects: &[AgentEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. }))
}

fn asks(effects: &[AgentEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::RequestPermission { .. }))
}

fn events(effects: &[AgentEffect]) -> Vec<&AgentEvent> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            AgentEffect::Emit { event } => Some(event),
            _ => None,
        })
        .collect()
}

fn denial(effects: &[AgentEffect]) -> Option<(Option<String>, String)> {
    events(effects).into_iter().find_map(|event| match event {
        AgentEvent::ToolCallDenied {
            rule_id, reason, ..
        } => Some((rule_id.clone(), reason.clone())),
        _ => None,
    })
}

fn result_text(messages: &[AgentMessage], call_id: ToolCallId) -> String {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                call_id: id,
                result,
            } if *id == call_id => Some(result.output_preview.clone()),
            _ => None,
        })
        .expect("result recorded")
}

fn tail_text(request: &ExecutionRequest) -> String {
    match request.messages.last().unwrap().content.last().unwrap() {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::ToolResult { result, .. } => result.output_preview.clone(),
        other => panic!("unexpected tail {other:?}"),
    }
}

#[test]
fn ordering_is_enforced_with_a_reason_the_model_reads() {
    let mut agent = agent(json!([
        { "id": "fetch-before-edit", "on": "PreToolUse",
          "when": { "all": [ { "tool": "fs.edit" }, { "calls": { "tool": "smart_fetch", "eq": 0 } } ] },
          "do": { "deny": { "reason": "Gather the data with smart_fetch before editing." } } }
    ]));
    let (run_id, _) = start(&mut agent);

    let (denied_call, effects) = tool(&mut agent, run_id, "fs.edit", json!({"path": "a.rs"}));
    assert!(!executes(&effects));
    assert_eq!(
        denial(&effects),
        Some((
            Some("fetch-before-edit".into()),
            "Gather the data with smart_fetch before editing.".into()
        ))
    );
    assert!(events(&effects).iter().any(|event| matches!(event,
        AgentEvent::BehaviorRuleFired { rule_id, event, action, profile }
            if rule_id == "fetch-before-edit" && event == "PreToolUse" && action == "deny" && profile == "rules@1")));
    assert_eq!(
        result_text(&agent.state.messages, denied_call),
        "Denied: Gather the data with smart_fetch before editing."
    );

    let (fetch, effects) = tool(&mut agent, run_id, "smart_fetch", json!({"q": "a"}));
    assert!(executes(&effects));
    end_turn(&mut agent, run_id, "tool_use");
    complete(&mut agent, fetch, json!("data"), false);

    let (_, effects) = tool(&mut agent, run_id, "fs.edit", json!({"path": "a.rs"}));
    assert!(executes(&effects), "allowed once the data was fetched");
}

#[test]
fn before_request_reminders_reach_the_request_but_not_the_transcript() {
    let mut agent = agent(json!([
        { "id": "decide-first", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "infer_decision", "eq": 0 } },
          "do": { "inject": { "text": "Start by calling infer_decision." } } }
    ]));
    let (run_id, effects) = start(&mut agent);
    let first = request(&effects).unwrap();
    assert_eq!(
        tail_text(first),
        "<system-reminder source=\"profile:rules@1 rule:decide-first\">Start by calling infer_decision.</system-reminder>"
    );
    assert!(events(&effects).iter().any(|event| matches!(event,
        AgentEvent::ContextInjected { placement, .. } if placement == "next_request")));
    assert_eq!(
        agent.state.messages.last().unwrap().content.len(),
        1,
        "the stored user message is untouched"
    );

    let (decision, _) = tool(&mut agent, run_id, "infer_decision", json!({}));
    end_turn(&mut agent, run_id, "tool_use");
    let effects = complete(&mut agent, decision, json!("plan"), false);
    let second = request(&effects).unwrap();
    assert_eq!(
        tail_text(second),
        "\"plan\"",
        "no reminder once the tool was used"
    );
}

#[test]
fn post_tool_context_is_stored_with_the_result() {
    let mut agent = agent(json!([
        { "id": "test-after-edit", "on": "PostToolUse", "when": { "tool": "fs.edit" },
          "do": { "inject": { "text": "Run the tests before claiming completion." } } },
        { "id": "on-failure", "on": "PostToolUseFailure",
          "do": { "inject": { "text": "Read the error before retrying." } } }
    ]));
    let (run_id, _) = start(&mut agent);
    let (edit, _) = tool(&mut agent, run_id, "fs.edit", json!({}));
    let (read, _) = tool(&mut agent, run_id, "fs.read", json!({}));
    end_turn(&mut agent, run_id, "tool_use");
    complete(&mut agent, edit, json!("ok"), false);
    complete(&mut agent, read, json!("boom"), true);

    assert_eq!(
        result_text(&agent.state.messages, edit),
        "\"ok\"\n\n<system-reminder source=\"profile:rules@1 rule:test-after-edit\">Run the tests before claiming completion.</system-reminder>"
    );
    assert!(result_text(&agent.state.messages, read).ends_with(
        "<system-reminder source=\"profile:rules@1 rule:on-failure\">Read the error before retrying.</system-reminder>"
    ));
}

#[test]
fn run_start_context_can_persist_in_the_user_message() {
    let mut agent = agent(json!([
        { "id": "greet", "on": "RunStart",
          "do": { "inject": { "text": "Mind the conventions.", "placement": "persistent" } } }
    ]));
    start(&mut agent);
    let user = agent
        .state
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .unwrap();
    assert!(matches!(&user.content[1], ContentBlock::Text { text }
        if text.contains("Mind the conventions.")));
}

#[test]
fn ask_and_allow_adjust_permission_but_never_override_a_denial() {
    let mut agent = agent(json!([
        { "id": "billing", "on": "PreToolUse",
          "when": { "arg": { "pointer": "/path", "glob": "src/billing/**" } },
          "do": { "ask": {} } },
        { "id": "trusted-shell", "on": "PreToolUse",
          "when": { "all": [ { "tool": "shell.exec" }, { "arg": { "pointer": "/command", "equals": "ls" } } ] },
          "do": { "allow": {} } },
        { "id": "allow-everything", "on": "PreToolUse", "when": { "tool": "rm" },
          "do": { "allow": {} } }
    ]));
    let (run_id, _) = start(&mut agent);

    let (_, effects) = tool(
        &mut agent,
        run_id,
        "fs.edit",
        json!({"path": "src/billing/tax.rs"}),
    );
    assert!(asks(&effects) && !executes(&effects), "allow → ask");

    let (_, effects) = tool(&mut agent, run_id, "shell.exec", json!({"command": "ls"}));
    assert!(executes(&effects) && !asks(&effects), "ask → allow");
    let (_, effects) = tool(&mut agent, run_id, "shell.exec", json!({"command": "make"}));
    assert!(asks(&effects), "other commands still ask");

    let (_, effects) = tool(&mut agent, run_id, "rm", json!({}));
    assert!(
        !executes(&effects) && !asks(&effects),
        "the session's deny stands"
    );
}

#[test]
fn repeated_identical_calls_are_stopped() {
    let mut agent = agent(json!([
        { "id": "no-loops", "on": "PreToolUse", "when": { "repeated_call": { "gte": 3 } },
          "do": { "deny": { "reason": "You made this exact call 3 times. Change approach." } } }
    ]));
    let (run_id, _) = start(&mut agent);
    for attempt in 1..=3 {
        let (_, effects) = tool(&mut agent, run_id, "fs.read", json!({"path": "same"}));
        assert_eq!(executes(&effects), attempt < 3, "attempt {attempt}");
    }
}

#[test]
fn stop_run_fails_cleanly_and_the_next_run_can_start() {
    let mut agent = agent(json!([
        { "id": "halt", "on": "PreToolUse", "when": { "tool": "shell.exec" },
          "do": { "stop_run": { "reason": "Shell is off limits here." } } }
    ]));
    let (run_id, _) = start(&mut agent);
    let (call, effects) = tool(&mut agent, run_id, "shell.exec", json!({}));
    assert_eq!(agent.state.status, AgentStatus::Failed);
    assert!(events(&effects).iter().any(|event| matches!(event,
        AgentEvent::Failed { error } if error.code == "BEHAVIOR_STOP" && error.message.contains("halt"))));
    assert!(result_text(&agent.state.messages, call).starts_with("Denied:"));

    // The transcript stayed valid: a new run starts normally.
    let (_, effects) = start(&mut agent);
    assert!(request(&effects).is_some());
}

#[test]
fn stop_run_after_a_tool_result_records_the_result_first() {
    let mut agent = agent(json!([
        { "id": "fatal", "on": "PostToolUseFailure", "when": { "result_contains": "FATAL" },
          "do": { "stop_run": { "reason": "Fatal tool error." } } }
    ]));
    let (run_id, _) = start(&mut agent);
    let (call, _) = tool(&mut agent, run_id, "fs.read", json!({}));
    end_turn(&mut agent, run_id, "tool_use");
    let effects = complete(&mut agent, call, json!("FATAL: disk"), true);
    assert_eq!(agent.state.status, AgentStatus::Failed);
    assert!(request(&effects).is_none(), "no further model request");
    assert!(result_text(&agent.state.messages, call).contains("FATAL"));
}

#[test]
fn user_denials_are_not_counted_as_executed_calls() {
    let mut agent = agent(json!([
        { "id": "after-shell", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "shell.exec", "gte": 1 } },
          "do": { "inject": { "text": "shell ran" } } }
    ]));
    let (run_id, _) = start(&mut agent);
    let (_, effects) = tool(&mut agent, run_id, "shell.exec", json!({}));
    let permission = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::RequestPermission { request } => Some(request.id),
            _ => None,
        })
        .unwrap();
    end_turn(&mut agent, run_id, "tool_use");
    let effects = agent.apply(AgentCommand::PermissionResolved {
        id: permission,
        decision: PermissionDecision::Denied,
    });
    let next = request(&effects).expect("the loop continues");
    assert!(!tail_text(next).contains("shell ran"));
}

#[test]
fn a_reissued_request_repeats_its_reminders() {
    let mut agent = agent(json!([
        { "id": "remind", "on": "BeforeModelRequest", "max_fires": 1,
          "do": { "inject": { "text": "Plan first." } } }
    ]));
    let (_, effects) = start(&mut agent);
    let first = tail_text(request(&effects).unwrap());
    assert!(first.contains("Plan first."));

    agent.apply(AgentCommand::Pause);
    let effects = agent.apply(AgentCommand::Resume);
    let reissued = request(&effects).expect("resume re-issues the request");
    assert_eq!(tail_text(reissued), first, "same turn, same context");
    assert_eq!(
        agent.state.behavior.run.turns, 1,
        "a re-issue is not a new turn"
    );
}

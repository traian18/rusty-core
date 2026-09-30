//! `rewrite_args` and `rewrite_result` in the deterministic state machine.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, UserInput};
use harness_protocol::effects::AgentEffect;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, RunId, SessionId, ToolCallId,
    ToolId,
};
use harness_protocol::messages::ContentBlock;
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy, ToolResult,
};
use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};
use serde_json::{json, Value};

fn agent(rules: Value) -> Agent {
    let tools = [
        ("shell.exec", PermissionMode::Ask),
        ("fs.read", PermissionMode::Allow),
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
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    );
    agent.apply(AgentCommand::SetBehaviorProfile {
        profile: json!({
            "schema_version": 1, "id": "rewriting", "revision": 1, "name": "Rewriting",
            "rules": rules
        }),
        library: Vec::new(),
        allow_commands: false,
    });
    agent
}

fn start(agent: &mut Agent) -> RunId {
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "go".into(),
            attachments: vec![],
        },
    });
    effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request.run_id),
            _ => None,
        })
        .unwrap()
}

fn request_tool(
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
    (
        id,
        agent.apply(AgentCommand::BackendEvent {
            run_id,
            event: ExecutionEvent::ToolCallRequested {
                request_id: RequestId::new(),
                call,
            },
        }),
    )
}

fn end_turn(agent: &mut Agent, run_id: RunId) {
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id,
            result: ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "tool_use".into(),
            },
        },
    });
}

fn complete(agent: &mut Agent, call_id: ToolCallId, output: Value) {
    agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output,
            is_error: false,
        },
    });
}

fn result_text(agent: &Agent, call_id: ToolCallId) -> String {
    agent
        .state
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                call_id: id,
                result,
            } if *id == call_id => Some(result.output_preview.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn rewritten_arguments_are_what_gets_approved_and_executed() {
    let mut agent = agent(json!([
        { "id": "dry-run", "on": "PreToolUse", "when": { "tool": "shell.exec" },
          "do": { "rewrite_args": { "merge": { "dry_run": true, "env": { "CI": "1" } } } } },
        { "id": "no-hidden", "on": "PreToolUse", "when": { "tool": "fs.read" },
          "do": { "rewrite_args": { "merge": { "include_hidden": null } } } }
    ]));
    let run_id = start(&mut agent);

    let (shell, effects) = request_tool(
        &mut agent,
        run_id,
        "shell.exec",
        json!({"command": "make deploy"}),
    );
    let approval = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::RequestPermission { request } => Some(request.tool_call.arguments.clone()),
            _ => None,
        })
        .expect("shell still asks");
    let expected = json!({ "command": "make deploy", "dry_run": true, "env": { "CI": "1" } });
    assert_eq!(
        approval, expected,
        "the user approves what will actually run"
    );
    assert_eq!(agent.state.pending_tools[&shell].call.arguments, expected);

    let (read, effects) = request_tool(
        &mut agent,
        run_id,
        "fs.read",
        json!({"path": "a", "include_hidden": true}),
    );
    let executed = effects
        .iter()
        .find_map(|effect| match effect {
            AgentEffect::ExecuteTool { request } => Some(request.call.arguments.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(executed, json!({ "path": "a" }));

    end_turn(&mut agent, run_id);
    complete(&mut agent, read, json!("contents"));
    let text = result_text(&agent, read);
    assert!(text.starts_with("\"contents\""), "{text}");
    assert!(
        text.contains(
            "The harness adjusted this call's arguments before running it: {\"path\":\"a\"}"
        ),
        "{text}"
    );
    // The model's own tool_use block keeps what it asked for.
    let asked = agent
        .state
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolUse { call } if call.id == read => Some(call.arguments.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(asked, json!({"path": "a", "include_hidden": true}));
}

#[test]
fn results_can_be_replaced_or_extended() {
    let mut agent = agent(json!([
        { "id": "redact", "on": "PostToolUse", "when": { "result_contains": "API_KEY" },
          "do": { "rewrite_result": { "replace": "[redacted: the file contains credentials]" } } },
        { "id": "footer", "on": "PostToolUse", "when": { "tool": "fs.read" },
          "do": { "rewrite_result": { "append": "(read-only workspace)" } } }
    ]));
    let run_id = start(&mut agent);
    let (secret, _) = request_tool(&mut agent, run_id, "fs.read", json!({"path": ".env"}));
    let (plain, _) = request_tool(&mut agent, run_id, "fs.read", json!({"path": "README"}));
    end_turn(&mut agent, run_id);
    complete(&mut agent, secret, json!("API_KEY=abc"));
    complete(&mut agent, plain, json!("hello"));
    assert_eq!(
        result_text(&agent, secret),
        "[redacted: the file contains credentials]\n\n(read-only workspace)"
    );
    assert_eq!(
        result_text(&agent, plain),
        "\"hello\"\n\n(read-only workspace)"
    );
}

//! Behavior profiles applied by the deterministic agent state machine:
//! tool scope at description and execution, permission tightening, request
//! shaping, per-run limits, and profile changes.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::behavior::{
    default_profile_definition, BehaviorProfile, ExecutionOverlay, Limits, ProfileId, ToolOverride,
    ToolPermission,
};
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_core::orchestration::ToolScope;
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionRequest, ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentStatus, UserInput};
use harness_protocol::effects::AgentEffect;
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, RunId, SessionId, ToolCallId,
    ToolId,
};
use harness_protocol::messages::ContentBlock;
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy, ToolResult,
};
use harness_protocol::usage::{AgentBudget, Cost, ModelUsage};
use serde_json::json;

fn tool(name: &str, permission: PermissionMode) -> (ToolId, ToolCapability) {
    let id = ToolId::new();
    (
        id,
        ToolCapability {
            descriptor: ToolDescriptor {
                id,
                name: name.into(),
                description: format!("{name} tool"),
                input_schema: json!({"type": "object"}),
            },
            policy: ToolPolicy {
                permission,
                enabled: true,
            },
            delegatable: true,
        },
    )
}

fn agent() -> Agent {
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        "Session prompt.".into(),
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
                tools: HashMap::from([
                    tool("fs.read", PermissionMode::Allow),
                    tool("fs.edit", PermissionMode::Allow),
                    tool("shell.exec", PermissionMode::Ask),
                ]),
            },
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
    )
}

fn profile(edit: impl FnOnce(&mut BehaviorProfile)) -> BehaviorProfile {
    let mut profile = BehaviorProfile {
        id: ProfileId::from("test"),
        name: "Test".into(),
        ..default_profile_definition()
    };
    edit(&mut profile);
    profile
}

fn set_profile(agent: &mut Agent, profile: BehaviorProfile) {
    let effects = agent.apply(AgentCommand::SetBehaviorProfile {
        library: Vec::new(),
        allow_commands: false,
        profile: serde_json::to_value(profile).unwrap(),
    });
    // Idle agents switch at once and announce it; busy ones defer silently.
    assert!(effects.iter().all(|effect| matches!(
        effect,
        AgentEffect::Emit {
            event: AgentEvent::ProfileChanged { .. }
        }
    )));
}

fn requests(effects: &[AgentEffect]) -> Vec<&ExecutionRequest> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            AgentEffect::ExecuteBackend { request } => Some(request),
            _ => None,
        })
        .collect()
}

fn start(agent: &mut Agent) -> (RunId, ExecutionRequest) {
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "go".into(),
            attachments: vec![],
        },
    });
    let request = requests(&effects)
        .first()
        .cloned()
        .cloned()
        .expect("first request");
    (request.run_id, request)
}

fn tool_names(request: &ExecutionRequest) -> Vec<String> {
    let mut names: Vec<_> = request.tools.iter().map(|tool| tool.name.clone()).collect();
    names.sort();
    names
}

fn request_tool(agent: &mut Agent, run_id: RunId, name: &str) -> (ToolCallId, Vec<AgentEffect>) {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: name.into(),
        arguments: json!({}),
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

fn complete_tool(agent: &mut Agent, call_id: ToolCallId) -> Vec<AgentEffect> {
    agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output: json!("ok"),
            is_error: false,
        },
    })
}

fn executes_tool(effects: &[AgentEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::ExecuteTool { .. }))
}

/// The harness refused the call and told the model why.
fn denied(effects: &[AgentEffect]) -> bool {
    let announced = effects.iter().any(|effect| {
        matches!(
            effect,
            AgentEffect::Emit {
                event: AgentEvent::ToolCallDenied { .. }
            }
        )
    });
    let explained = effects.iter().any(|effect| {
        matches!(effect, AgentEffect::Emit { event: AgentEvent::ToolCallCompleted { result, .. } }
            if result.has_error && result.output_preview.starts_with("Denied: "))
    });
    announced && explained
}

#[test]
fn without_a_profile_requests_are_unchanged() {
    let mut agent = agent();
    let (_, request) = start(&mut agent);
    assert_eq!(request.system_prompt, "Session prompt.");
    assert_eq!(tool_names(&request), ["fs.edit", "fs.read", "shell.exec"]);
    assert_eq!(agent.state.behavior.run.turns, 1);
}

#[test]
fn profile_shapes_the_request() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| {
            profile.instructions.text = "Profile rules.".into();
            profile.tools = ToolScope::AllowList(vec!["fs.read".into(), "fs.edit".into()]);
            profile.tool_overrides.insert(
                "fs.edit".into(),
                ToolOverride {
                    permission: None,
                    description_append: Some("Read before editing.".into()),
                },
            );
            profile.execution = ExecutionOverlay {
                model: Some("small".into()),
                temperature: Some(0.1),
                ..Default::default()
            };
        }),
    );
    let (_, request) = start(&mut agent);

    assert_eq!(request.system_prompt, "Session prompt.\n\nProfile rules.");
    assert_eq!(tool_names(&request), ["fs.edit", "fs.read"]);
    let edit = request
        .tools
        .iter()
        .find(|tool| tool.name == "fs.edit")
        .unwrap();
    assert_eq!(edit.description, "fs.edit tool\n\nRead before editing.");
    assert_eq!(request.params.model.as_deref(), Some("small"));
    assert_eq!(request.params.temperature, Some(0.1));
    assert_eq!(
        agent.state.system_prompt, "Session prompt.",
        "the agent's own prompt is untouched"
    );
}

#[test]
fn hidden_tools_are_not_executable_by_name() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| profile.tools = ToolScope::AllowList(vec!["fs.read".into()])),
    );
    let (run_id, _) = start(&mut agent);
    let (_, effects) = request_tool(&mut agent, run_id, "shell.exec");
    assert!(!executes_tool(&effects));
    assert!(denied(&effects));
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::RequestPermission { .. })));
    assert_eq!(
        agent.state.behavior.run.tool_calls, 0,
        "denials are not counted"
    );

    let (_, effects) = request_tool(&mut agent, run_id, "fs.read");
    assert!(executes_tool(&effects));
}

#[test]
fn overrides_tighten_permissions() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| {
            profile.tool_overrides.insert(
                "fs.edit".into(),
                ToolOverride {
                    permission: Some(ToolPermission::Ask),
                    description_append: None,
                },
            );
            profile.tool_overrides.insert(
                "shell.exec".into(),
                ToolOverride {
                    permission: Some(ToolPermission::Allow),
                    description_append: None,
                },
            );
        }),
    );
    let (run_id, _) = start(&mut agent);
    let (_, effects) = request_tool(&mut agent, run_id, "fs.edit");
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, AgentEffect::RequestPermission { .. })));
    assert_eq!(agent.state.status, AgentStatus::WaitingForPermission);

    let (_, effects) = request_tool(&mut agent, run_id, "shell.exec");
    assert!(
        !executes_tool(&effects),
        "an `allow` override cannot skip the session's `ask`"
    );
}

#[test]
fn the_last_allowed_turn_offers_no_tools_and_carries_the_final_prompt() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| {
            profile.limits = Limits {
                max_turns: Some(2),
                max_tool_calls: None,
                final_turn_prompt: Some("Summarize now.".into()),
            };
        }),
    );
    let (run_id, first) = start(&mut agent);
    assert_eq!(first.tools.len(), 3);

    let (call_id, effects) = request_tool(&mut agent, run_id, "fs.read");
    assert!(executes_tool(&effects));
    end_turn(&mut agent, run_id, "tool_use");
    let effects = complete_tool(&mut agent, call_id);
    let second = requests(&effects)[0];

    assert!(second.tools.is_empty());
    let Some(ContentBlock::ToolResult { result, .. }) =
        second.messages.last().unwrap().content.last()
    else {
        panic!("request should end with the tool result");
    };
    assert!(result
        .output_preview
        .ends_with("<system-reminder source=\"profile:test@1\">Summarize now.</system-reminder>"));
    // The transcript itself never contains the injected text.
    assert!(agent.state.messages.iter().all(|message| message.content.iter().all(|block| {
        !matches!(block, ContentBlock::ToolResult { result, .. } if result.output_preview.contains("system-reminder"))
    })));
    assert!(agent.state.behavior.run.final_turn);
}

#[test]
fn tool_calls_after_the_final_turn_fail_the_run() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| profile.limits.max_turns = Some(1)),
    );
    let (run_id, first) = start(&mut agent);
    assert!(first.tools.is_empty(), "a one-turn run offers no tools");

    let (call_id, effects) = request_tool(&mut agent, run_id, "fs.read");
    assert!(denied(&effects), "the model named a tool anyway");
    let _ = call_id;
    let effects = end_turn(&mut agent, run_id, "tool_use");
    assert_eq!(agent.state.status, AgentStatus::Failed);
    assert!(effects.iter().any(|effect| matches!(effect,
        AgentEffect::Emit { event: AgentEvent::Failed { error } } if error.code == "BEHAVIOR_LIMIT_EXCEEDED")));
}

#[test]
fn the_tool_budget_refuses_extra_calls() {
    let mut agent = agent();
    set_profile(
        &mut agent,
        profile(|profile| profile.limits.max_tool_calls = Some(1)),
    );
    let (run_id, _) = start(&mut agent);
    let (_, first) = request_tool(&mut agent, run_id, "fs.read");
    let (_, second) = request_tool(&mut agent, run_id, "fs.read");
    assert!(executes_tool(&first));
    assert!(!executes_tool(&second));
    assert!(denied(&second));
}

#[test]
fn a_profile_set_mid_run_applies_to_the_next_run() {
    let mut agent = agent();
    let (run_id, _) = start(&mut agent);
    set_profile(
        &mut agent,
        profile(|profile| profile.tools = ToolScope::None),
    );
    assert_eq!(
        agent.state.behavior.profile.profile.id.as_str(),
        "rusty.default",
        "the active run keeps its profile"
    );
    end_turn(&mut agent, run_id, "end_turn");
    assert_eq!(agent.state.status, AgentStatus::Idle);

    let (_, request) = start(&mut agent);
    assert!(request.tools.is_empty());
    assert_eq!(agent.state.behavior.profile.profile.id.as_str(), "test");
    assert!(agent.state.pending_profile.is_none());
}

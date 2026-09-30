//! Profile switching (modes) in the deterministic state machine: rule-driven
//! and host-driven switches, the announcement, `ProfileEntered` rules, and
//! the switch-loop guard.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
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
use serde_json::{json, Value};

fn agent() -> Agent {
    let tools = ["fs.read", "fs.edit", "submit_plan"]
        .into_iter()
        .map(|name| {
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
                        permission: PermissionMode::Allow,
                        enabled: true,
                    },
                    delegatable: true,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    Agent::new(
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
    )
}

fn plan() -> Value {
    json!({
        "schema_version": 1, "id": "plan", "revision": 1, "name": "Plan",
        "instructions": { "text": "PLAN MODE: read only." },
        "tools": { "type": "allow_list", "tools": ["fs.read", "submit_plan"] },
        "rules": [
            { "id": "to-build", "on": "PostToolUse", "when": { "tool": "submit_plan" },
              "do": { "switch_profile": { "profile": { "id": "build" } } } }
        ]
    })
}

fn build() -> Value {
    json!({
        "schema_version": 1, "id": "build", "revision": 1, "name": "Build",
        "instructions": { "text": "BUILD MODE: implement the plan." },
        "rules": [
            { "id": "execute-plan", "on": "ProfileEntered",
              "when": { "profile_entered_from": "plan" },
              "do": { "inject": { "text": "Execute the approved plan." } } }
        ]
    })
}

fn install(agent: &mut Agent, profile: Value, library: Vec<Value>) -> Vec<AgentEffect> {
    agent.apply(AgentCommand::SetBehaviorProfile {
        profile,
        library,
        allow_commands: false,
    })
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

fn start(agent: &mut Agent) -> (RunId, Vec<AgentEffect>) {
    let effects = agent.apply(AgentCommand::StartRun {
        input: UserInput {
            text: "add a feature".into(),
            attachments: vec![],
        },
    });
    (requests(&effects)[0].run_id, effects)
}

fn call_and_complete(agent: &mut Agent, run_id: RunId, tool: &str) -> Vec<AgentEffect> {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: tool.into(),
        arguments: json!({}),
    };
    let call_id = call.id;
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::ToolCallRequested { request_id, call },
    });
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
    agent.apply(AgentCommand::ToolCompleted {
        call_id,
        result: ToolResult {
            call_id,
            output: json!("ok"),
            is_error: false,
        },
    })
}

fn tool_names(request: &ExecutionRequest) -> Vec<String> {
    let mut names: Vec<_> = request.tools.iter().map(|tool| tool.name.clone()).collect();
    names.sort();
    names
}

fn tail(request: &ExecutionRequest) -> String {
    match request.messages.last().unwrap().content.last().unwrap() {
        ContentBlock::ToolResult { result, .. } => result.output_preview.clone(),
        ContentBlock::Text { text } => text.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

fn profile_changes(effects: &[AgentEffect]) -> Vec<(String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            AgentEffect::Emit {
                event: AgentEvent::ProfileChanged { from, to },
            } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_rule_switches_plan_to_build_before_the_next_request() {
    let mut agent = agent();
    install(&mut agent, plan(), vec![build()]);
    let (run_id, effects) = start(&mut agent);
    let first = requests(&effects)[0];
    assert_eq!(tool_names(first), ["fs.read", "submit_plan"]);
    assert_eq!(first.system_prompt, "PLAN MODE: read only.");

    let effects = call_and_complete(&mut agent, run_id, "submit_plan");
    assert_eq!(
        profile_changes(&effects),
        [("plan@1".to_string(), "build@1".to_string())]
    );
    let second = requests(&effects)[0];
    assert_eq!(tool_names(second), ["fs.edit", "fs.read", "submit_plan"]);
    assert_eq!(second.system_prompt, "BUILD MODE: implement the plan.");
    let tail = tail(second);
    assert!(
        tail.contains("Your operating mode changed from Plan to Build. Tools available now: fs.edit, fs.read, submit_plan."),
        "{tail}"
    );
    assert!(tail.contains("Execute the approved plan."), "{tail}");
    assert!(effects.iter().any(|effect| matches!(effect,
        AgentEffect::Emit { event: AgentEvent::BehaviorRuleFired { rule_id, event, .. } }
            if rule_id == "execute-plan" && event == "ProfileEntered")));

    assert_eq!(agent.state.behavior.reference().to_string(), "build@1");
    assert_eq!(
        agent.state.behavior.run.turns, 1,
        "counters restart under the new profile"
    );
    assert_eq!(agent.state.behavior.run.switches, 1);
}

#[test]
fn a_host_switch_mid_run_applies_at_the_next_request() {
    let mut agent = agent();
    install(&mut agent, plan(), vec![build()]);
    let (run_id, _) = start(&mut agent);

    assert!(
        install(&mut agent, build(), vec![]).is_empty(),
        "deferred while running"
    );
    assert_eq!(agent.state.behavior.reference().to_string(), "plan@1");

    let effects = call_and_complete(&mut agent, run_id, "fs.read");
    assert_eq!(profile_changes(&effects).len(), 1);
    let next = requests(&effects)[0];
    assert_eq!(next.system_prompt, "BUILD MODE: implement the plan.");
    assert!(tail(next).contains("from Plan to Build"));
}

#[test]
fn switching_between_runs_announces_only_when_there_is_history() {
    let mut agent = agent();
    let changes = profile_changes(&install(&mut agent, plan(), vec![build()]));
    assert_eq!(
        changes,
        [("rusty.default@1".to_string(), "plan@1".to_string())]
    );
    let (run_id, effects) = start(&mut agent);
    assert!(
        !tail(requests(&effects)[0]).contains("operating mode"),
        "nothing to reinterpret on a fresh conversation"
    );
    let request_id = RequestId::new();
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::TextDelta {
            request_id,
            delta: "Here is the plan.".into(),
        },
    });
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id,
            result: ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            },
        },
    });
    assert_eq!(agent.state.status, AgentStatus::Idle);

    install(&mut agent, build(), vec![]);
    let (_, effects) = start(&mut agent);
    let request = requests(&effects)[0];
    let last = format!("{:?}", request.messages.last().unwrap());
    assert!(last.contains("changed from Plan to Build"), "{last}");
    assert!(
        last.contains("Execute the approved plan."),
        "entered rules fire too"
    );
}

#[test]
fn bouncing_between_profiles_is_stopped() {
    let ping = json!({
        "schema_version": 1, "id": "ping", "revision": 1, "name": "Ping",
        "rules": [{ "id": "go", "on": "BeforeModelRequest",
                    "do": { "switch_profile": { "profile": { "id": "pong" } } } }]
    });
    let pong = json!({
        "schema_version": 1, "id": "pong", "revision": 1, "name": "Pong",
        "rules": [{ "id": "back", "on": "BeforeModelRequest",
                    "do": { "switch_profile": { "profile": { "id": "ping" } } } }]
    });
    let mut agent = agent();
    install(&mut agent, ping, vec![pong]);
    let (run_id, _) = start(&mut agent);
    for _ in 0..40 {
        if agent.state.status == AgentStatus::Failed {
            break;
        }
        let effects = call_and_complete(&mut agent, run_id, "fs.read");
        if let Some(error) = effects.iter().find_map(|effect| match effect {
            AgentEffect::Emit {
                event: AgentEvent::Failed { error },
            } => Some(error.clone()),
            _ => None,
        }) {
            assert_eq!(error.code, "BEHAVIOR_SWITCH_LOOP");
        }
    }
    assert_eq!(agent.state.status, AgentStatus::Failed);
}

#[test]
fn the_library_survives_a_snapshot_and_is_inherited_by_children() {
    use harness_core::behavior::{
        BehaviorState, ChildBehaviorResolver, PolicyChildBehaviorResolver, ProfileRef,
    };
    let mut agent = agent();
    install(&mut agent, plan(), vec![build()]);
    let behavior = &agent.state.behavior;
    assert!(behavior
        .resolve_switch(&ProfileRef::latest("build"))
        .is_some());

    let restored = BehaviorState::from_stored(Some(&behavior.to_stored())).unwrap();
    assert!(restored
        .resolve_switch(&ProfileRef::latest("build"))
        .is_some());
    assert_eq!(restored.entered_from, behavior.entered_from);

    let spec = serde_json::from_value(json!({
        "role": null, "backend": "Inherit", "tools": "InheritAll", "workspace": "Inherit",
        "budget": {}, "mode": "Concurrent"
    }))
    .unwrap();
    let child = PolicyChildBehaviorResolver
        .resolve(behavior, &spec)
        .unwrap();
    assert!(child.resolve_switch(&ProfileRef::latest("build")).is_some());
}

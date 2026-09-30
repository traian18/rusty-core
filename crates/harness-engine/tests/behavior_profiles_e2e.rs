//! Behavior profiles through the public engine API: host registry, workspace
//! profiles and their default, precedence, switching, and orchestration
//! steps running under a named profile.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_core::behavior::{ProfileRef, DEFAULT_PROFILE_ID};
use harness_core::orchestration::{
    default_orchestration_definition, DefinitionRef, OrchestrationDefinitionId,
    OrchestrationNodeKind, OrchestrationOutcome, OrchestrationRunId,
};
use harness_engine::{Harness, HarnessError, OrchestrationConfig, OrchestrationRequest};
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::RequestId;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use harness_runtime::traits::ExecutionBackend;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Answers every request with `reply` and records the requests it saw.
struct RecordingBackend {
    requests: Arc<Mutex<Vec<ExecutionRequest>>>,
    inner: FakeBackend,
}

impl RecordingBackend {
    fn new(reply: &str) -> (Arc<Self>, Arc<Mutex<Vec<ExecutionRequest>>>) {
        let request_id = RequestId::new();
        let result = ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(Self {
            requests: requests.clone(),
            inner: FakeBackend::new()
                .with_events(vec![
                    ExecutionEvent::TextDelta {
                        request_id,
                        delta: reply.into(),
                    },
                    ExecutionEvent::Completed {
                        request_id,
                        result: result.clone(),
                    },
                ])
                .with_result(result),
        });
        (backend, requests)
    }
}

#[async_trait]
impl ExecutionBackend for RecordingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.inner.descriptor()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.execute(request, sink, cancel).await
    }
}

fn profile(id: &str, revision: u64, text: &str) -> Value {
    json!({
        "schema_version": 1, "id": id, "revision": revision, "name": id,
        "instructions": { "text": text }
    })
}

/// The root agent applies profile commands asynchronously; wait for it.
async fn wait_for_profile(session: &harness_engine::SessionHandle, expected: ProfileRef) {
    let mut last = None;
    for _ in 0..200 {
        let current = session.behavior_profile().await.unwrap();
        if current == expected {
            return;
        }
        last = Some(current);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("expected profile {expected}, still {last:?}");
}

async fn prompt_and_wait(
    session: &harness_engine::SessionHandle,
    requests: &Arc<Mutex<Vec<ExecutionRequest>>>,
) -> ExecutionRequest {
    let before = requests.lock().unwrap().len();
    session.send("hello").await.unwrap();
    for _ in 0..200 {
        if let Some(request) = requests.lock().unwrap().get(before) {
            return request.clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no request was made");
}

fn workspace_with_profiles(files: &[(&str, Value)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join(".rusty/profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    for (name, contents) in files {
        std::fs::write(profiles.join(name), contents.to_string()).unwrap();
    }
    dir
}

#[tokio::test]
async fn sessions_run_under_the_builtin_default_unless_told_otherwise() {
    let harness = Harness::new();
    let (backend, requests) = RecordingBackend::new("hi");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact(DEFAULT_PROFILE_ID, 1)).await;
    let request = prompt_and_wait(&session, &requests).await;
    assert_eq!(request.system_prompt, "", "the default adds nothing");
}

#[tokio::test]
async fn host_registered_profiles_can_be_chosen_and_switched() {
    let harness = Harness::new();
    let reviewer = harness
        .profiles()
        .register_json(profile("reviewer", 1, "Review carefully."))
        .unwrap();
    harness
        .profiles()
        .register_json(profile("writer", 1, "Write boldly."))
        .unwrap();

    let (backend, requests) = RecordingBackend::new("hi");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .profile(reviewer.clone())
        .start()
        .await
        .unwrap();
    wait_for_profile(&session, reviewer).await;
    assert_eq!(
        prompt_and_wait(&session, &requests).await.system_prompt,
        "Review carefully."
    );

    session
        .set_behavior_profile(ProfileRef::latest("writer"))
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact("writer", 1)).await;
    assert_eq!(
        prompt_and_wait(&session, &requests).await.system_prompt,
        "Write boldly."
    );

    let error = session
        .set_behavior_profile(ProfileRef::latest("missing"))
        .await
        .unwrap_err();
    assert!(matches!(error, HarnessError::Profile(_)));
}

#[tokio::test]
async fn workspace_profiles_and_their_default_are_loaded_on_request() {
    let workspace = workspace_with_profiles(&[
        ("team.json", profile("team", 1, "Team rules.")),
        (
            "more.json",
            json!([
                profile("strict", 1, "Strict."),
                profile("strict", 2, "Stricter.")
            ]),
        ),
        ("config.json", json!({ "default": { "id": "team" } })),
    ]);
    let harness = Harness::new();

    let start = |explicit: Option<ProfileRef>| {
        let (backend, _) = RecordingBackend::new("hi");
        let mut builder = harness
            .session()
            .backend(backend)
            .tools(Arc::new(FakeToolRegistry::new()))
            .workspace_profiles(workspace.path());
        if let Some(reference) = explicit {
            builder = builder.profile(reference);
        }
        builder.start()
    };

    let session = start(None).await.unwrap();
    wait_for_profile(&session, ProfileRef::exact("team", 1)).await;

    let session = start(Some(ProfileRef::latest("strict"))).await.unwrap();
    wait_for_profile(&session, ProfileRef::exact("strict", 2)).await;

    // Without opting in, workspace files are not read.
    let (backend, _) = RecordingBackend::new("hi");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact(DEFAULT_PROFILE_ID, 1)).await;
}

#[tokio::test]
async fn host_profiles_win_over_workspace_profiles_with_the_same_revision() {
    let workspace = workspace_with_profiles(&[("team.json", profile("team", 1, "Workspace."))]);
    let harness = Harness::new();
    harness
        .profiles()
        .register_json(profile("team", 1, "Host."))
        .unwrap();
    let (backend, requests) = RecordingBackend::new("hi");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .workspace_profiles(workspace.path())
        .profile(ProfileRef::exact("team", 1))
        .start()
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact("team", 1)).await;
    assert_eq!(
        prompt_and_wait(&session, &requests).await.system_prompt,
        "Host."
    );
}

#[tokio::test]
async fn bad_profiles_fail_session_start() {
    let harness = Harness::new();
    let start = |workspace: Option<&std::path::Path>, explicit: Option<ProfileRef>| {
        let (backend, _) = RecordingBackend::new("hi");
        let mut builder = harness
            .session()
            .backend(backend)
            .tools(Arc::new(FakeToolRegistry::new()));
        if let Some(root) = workspace {
            builder = builder.workspace_profiles(root);
        }
        if let Some(reference) = explicit {
            builder = builder.profile(reference);
        }
        builder.start()
    };

    let unknown = start(None, Some(ProfileRef::latest("nope"))).await.err();
    assert!(matches!(unknown, Some(HarnessError::Profile(_))));

    let invalid = workspace_with_profiles(&[(
        "broken.json",
        json!({ "schema_version": 1, "id": "broken", "revision": 1, "name": "x", "tolls": [] }),
    )]);
    let error = start(Some(invalid.path()), None).await.err().unwrap();
    assert!(
        matches!(&error, HarnessError::Profile(message) if message.contains("broken.json")),
        "{error}"
    );

    let reserved = workspace_with_profiles(&[("mine.json", profile("rusty.default", 2, "x"))]);
    assert!(matches!(
        start(Some(reserved.path()), None).await.err(),
        Some(HarnessError::Profile(_))
    ));

    let bad_default =
        workspace_with_profiles(&[("config.json", json!({ "default": { "id": "ghost" } }))]);
    assert!(matches!(
        start(Some(bad_default.path()), None).await.err(),
        Some(HarnessError::Profile(_))
    ));
}

#[tokio::test]
async fn orchestration_steps_run_under_the_profile_they_name() {
    let report = json!({
        "summary": "reviewed", "status": "completed", "artifacts": [], "claimsToVerify": []
    });
    let harness = Harness::builder()
        .orchestration(OrchestrationConfig::default())
        .build()
        .await
        .unwrap();
    harness
        .profiles()
        .register_json(profile("step-reviewer", 1, "STEP PROFILE."))
        .unwrap();

    let mut definition = default_orchestration_definition();
    definition.id = OrchestrationDefinitionId::from("reviewed");
    for node in &mut definition.nodes {
        if let OrchestrationNodeKind::Agent(config) = &mut node.kind {
            config.profile = Some(ProfileRef::exact("step-reviewer", 1));
        }
    }
    harness
        .orchestration()
        .unwrap()
        .register(definition)
        .unwrap();

    let (backend, requests) = RecordingBackend::new(&report.to_string());
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .unwrap();
    let output = session
        .run_orchestration(
            OrchestrationRequest {
                run_id: OrchestrationRunId::from("reviewed-1"),
                definition: DefinitionRef::Exact {
                    id: OrchestrationDefinitionId::from("reviewed"),
                    revision: 1,
                },
                input: json!({ "request": "review it" }),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        output.result.status,
        OrchestrationOutcome::Completed,
        "{:?}",
        output.result
    );
    let step_request = requests.lock().unwrap()[0].clone();
    assert_eq!(step_request.system_prompt, "STEP PROFILE.");
}

#[tokio::test]
async fn orchestration_rejects_steps_naming_unknown_profiles_before_running() {
    let harness = Harness::builder()
        .orchestration(OrchestrationConfig::default())
        .build()
        .await
        .unwrap();
    let mut definition = default_orchestration_definition();
    definition.id = OrchestrationDefinitionId::from("ghostly");
    for node in &mut definition.nodes {
        if let OrchestrationNodeKind::Agent(config) = &mut node.kind {
            config.profile = Some(ProfileRef::latest("ghost"));
        }
    }
    harness
        .orchestration()
        .unwrap()
        .register(definition)
        .unwrap();
    let (backend, requests) = RecordingBackend::new("{}");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .unwrap();
    let error = session
        .start_orchestration(OrchestrationRequest {
            run_id: OrchestrationRunId::from("ghost-1"),
            definition: DefinitionRef::Exact {
                id: OrchestrationDefinitionId::from("ghostly"),
                revision: 1,
            },
            input: json!({ "request": "x" }),
        })
        .await
        .err()
        .expect("rejected");
    assert!(matches!(error, HarnessError::OrchestrationDefinition(_)));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn workspace_profile_rules_reach_the_backend() {
    let workspace = workspace_with_profiles(&[(
        "guided.json",
        json!({
            "schema_version": 1, "id": "guided", "revision": 1, "name": "Guided",
            "rules": [
                { "id": "first-turn", "on": "BeforeModelRequest", "when": { "turn": { "eq": 1 } },
                  "do": { "inject": { "text": "Plan before acting." } } }
            ]
        }),
    )]);
    let harness = Harness::new();
    let (backend, requests) = RecordingBackend::new("ok");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .workspace_profiles(workspace.path())
        .profile(ProfileRef::latest("guided"))
        .start()
        .await
        .unwrap();
    let mut events = session.subscribe();
    wait_for_profile(&session, ProfileRef::exact("guided", 1)).await;

    let request = prompt_and_wait(&session, &requests).await;
    let Some(harness_protocol::messages::ContentBlock::Text { text }) =
        request.messages.last().unwrap().content.last()
    else {
        panic!("request should end with text");
    };
    assert_eq!(
        text,
        "<system-reminder source=\"profile:guided@1 rule:first-turn\">Plan before acting.</system-reminder>"
    );

    let mut saw = (false, false);
    for _ in 0..200 {
        while let Ok(envelope) = events.try_recv() {
            match envelope.event {
                harness_protocol::events::AgentEvent::BehaviorRuleFired { rule_id, .. } => {
                    saw.0 |= rule_id == "first-turn"
                }
                harness_protocol::events::AgentEvent::ContextInjected { source, .. } => {
                    saw.1 |= source == "profile:guided@1 rule:first-turn"
                }
                _ => {}
            }
        }
        if saw == (true, true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(saw, (true, true), "rule events reach session subscribers");
}

#[tokio::test]
async fn a_workflow_step_that_never_passes_its_gate_fails_the_step() {
    let report = json!({
        "summary": "done", "status": "completed", "artifacts": [], "claimsToVerify": []
    });
    let harness = Harness::builder()
        .orchestration(OrchestrationConfig::default())
        .build()
        .await
        .unwrap();
    harness
        .profiles()
        .register_json(json!({
            "schema_version": 1, "id": "must-test", "revision": 1, "name": "Must test",
            "completion_gate": {
                "checks": [{
                    "id": "tested",
                    "require": { "calls": { "tool": "run_tests", "gte": 1 } },
                    "feedback": "Run the tests before finishing."
                }],
                "max_continuations": 1
            }
        }))
        .unwrap();

    let mut definition = default_orchestration_definition();
    definition.id = OrchestrationDefinitionId::from("gated-steps");
    for node in &mut definition.nodes {
        if let OrchestrationNodeKind::Agent(config) = &mut node.kind {
            config.profile = Some(ProfileRef::exact("must-test", 1));
        }
    }
    harness
        .orchestration()
        .unwrap()
        .register(definition)
        .unwrap();

    let (backend, requests) = RecordingBackend::new(&report.to_string());
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .unwrap();
    let output = session
        .run_orchestration(
            OrchestrationRequest {
                run_id: OrchestrationRunId::from("gated-1"),
                definition: DefinitionRef::Exact {
                    id: OrchestrationDefinitionId::from("gated-steps"),
                    revision: 1,
                },
                input: json!({ "request": "change something" }),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output.result.status, OrchestrationOutcome::Failed);
    let error = output.result.error.unwrap();
    assert_eq!(error.code, "completion_gate_not_passed", "{error:?}");
    let execute =
        &output.state.steps[&harness_core::orchestration::OrchestrationNodeId::from("execute")];
    assert_eq!(execute.attempts.len(), 2, "retried under the step's policy");
    // Each attempt: first answer, one continuation after the rejection.
    assert_eq!(requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn switch_targets_must_resolve_before_a_session_starts() {
    let harness = Harness::new();
    harness
        .profiles()
        .register_json(json!({
            "schema_version": 1, "id": "planner", "revision": 1, "name": "Planner",
            "rules": [{ "id": "go", "on": "PostToolUse", "when": { "tool": "submit_plan" },
                        "do": { "switch_profile": { "profile": { "id": "builder" } } } }]
        }))
        .unwrap();
    let (backend, _) = RecordingBackend::new("hi");
    let error = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .profile(ProfileRef::latest("planner"))
        .start()
        .await
        .err()
        .expect("builder is not registered");
    assert!(
        matches!(&error, HarnessError::Profile(message) if message.contains("builder")),
        "{error}"
    );
}

#[tokio::test]
async fn a_host_switch_is_announced_to_the_model() {
    let harness = Harness::new();
    harness
        .profiles()
        .register_json(profile("planner", 1, "Plan only."))
        .unwrap();
    harness
        .profiles()
        .register_json(profile("builder", 1, "Build it."))
        .unwrap();
    let (backend, requests) = RecordingBackend::new("Here is the plan.");
    let session = harness
        .session()
        .backend(backend)
        .tools(Arc::new(FakeToolRegistry::new()))
        .profile(ProfileRef::latest("planner"))
        .start()
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact("planner", 1)).await;
    let first = prompt_and_wait(&session, &requests).await;
    assert_eq!(first.system_prompt, "Plan only.");

    // Let the first run finish so the switch applies to the next one.
    for _ in 0..200 {
        if requests.lock().unwrap().len() == 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            break;
        }
    }
    session
        .set_behavior_profile(ProfileRef::latest("builder"))
        .await
        .unwrap();
    wait_for_profile(&session, ProfileRef::exact("builder", 1)).await;
    let second = prompt_and_wait(&session, &requests).await;
    assert_eq!(second.system_prompt, "Build it.");
    let last = format!("{:?}", second.messages.last().unwrap());
    assert!(last.contains("changed from planner to builder"), "{last}");
}

fn hooked(id: &str) -> Value {
    json!({
        "schema_version": 1, "id": id, "revision": 1, "name": id,
        "completion_gate": {
            "checks": [{ "id": "hook", "evaluator": { "type": "command", "command": "exit 0" } }]
        }
    })
}

#[tokio::test]
async fn command_evaluators_need_explicit_trust() {
    let harness = Harness::new();
    harness
        .profiles()
        .register_json(hooked("host-hooks"))
        .unwrap();
    let workspace = workspace_with_profiles(&[("hooks.json", hooked("repo-hooks"))]);

    let start = |profile: &str, allow: bool, trust_workspace: bool| {
        let (backend, _) = RecordingBackend::new("hi");
        harness
            .session()
            .backend(backend)
            .tools(Arc::new(FakeToolRegistry::new()))
            .workspace_profiles(workspace.path())
            .profile(ProfileRef::latest(profile))
            .allow_command_evaluators(allow)
            .trust_workspace_commands(trust_workspace)
            .start()
    };

    let error = start("host-hooks", false, false).await.err().unwrap();
    assert!(
        matches!(&error, HarnessError::Profile(message) if message.contains("allow_command_evaluators")),
        "{error}"
    );
    start("host-hooks", true, false)
        .await
        .expect("host profiles run commands once the session allows them");

    let error = start("repo-hooks", true, false).await.err().unwrap();
    assert!(
        matches!(&error, HarnessError::Profile(message) if message.contains("trust_workspace_commands")),
        "{error}"
    );
    start("repo-hooks", true, true)
        .await
        .expect("workspace profiles need workspace trust too");
}

#[test]
fn claude_code_and_codex_stop_hooks_import_as_gate_checks() {
    let settings = json!({
        "permissions": { "allow": ["Bash(npm test)"] },
        "hooks": {
            "Stop": [
                { "hooks": [
                    { "type": "command", "command": "./scripts/check.sh", "timeout": 30 },
                    { "type": "prompt", "prompt": "Did the agent finish every requested change?" },
                    { "type": "http", "url": "https://example.test" }
                ] },
                { "matcher": "", "hooks": [
                    { "type": "agent", "prompt": "Verify the tests were run." }
                ] }
            ],
            "PreToolUse": [
                { "matcher": "Bash", "hooks": [ { "type": "command", "command": "./guard.sh" } ] }
            ]
        }
    });
    let import = harness_engine::import_hooks(&settings).unwrap();
    assert_eq!(import.checks.len(), 3);
    assert_eq!(import.skipped.len(), 2, "{:?}", import.skipped);
    assert!(import
        .skipped
        .iter()
        .any(|reason| reason.contains("PreToolUse")));
    assert!(import
        .skipped
        .iter()
        .any(|reason| reason.contains("(http)")));

    let command = &import.checks[0];
    assert_eq!(command.id, "hook-stop-1");
    assert_eq!(
        serde_json::to_value(command.evaluator.as_ref().unwrap()).unwrap(),
        json!({ "type": "command", "command": "./scripts/check.sh", "timeout_ms": 30000 })
    );
    assert_eq!(
        command.error_policy,
        harness_core::behavior::ErrorPolicy::Pass,
        "a hook error does not block, as in Claude Code"
    );

    // The imported gate is a valid profile gate.
    let gate = import.into_gate(2).unwrap();
    let harness = Harness::new();
    harness
        .profiles()
        .register_json(json!({
            "schema_version": 1, "id": "imported", "revision": 1, "name": "Imported",
            "completion_gate": gate
        }))
        .expect("imported checks compile");
}

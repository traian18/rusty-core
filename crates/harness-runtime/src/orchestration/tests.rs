use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use harness_core::orchestration::{
    apply, compile, default_orchestration_definition, error_codes, AttemptDetails,
    CompiledOrchestration, DelegatedRunRef, OrchestrationCommand, OrchestrationDefinition,
    OrchestrationEvent, OrchestrationNodeId, OrchestrationNodeKind, OrchestrationOutcome,
    OrchestrationRunId, OrchestrationRunState, OrchestrationStatus, RetryReason, StepStatus,
    ToolScope,
};
use harness_protocol::commands::PermissionDecision;
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;

use super::*;

// ---------------------------------------------------------------------------
// Scripted agent
// ---------------------------------------------------------------------------

enum Behavior {
    Return(Result<Value, AgentExecutionError>),
    /// Ask for a permission, then return `Value` once a decision arrives.
    AskPermission(Value),
    /// Wait until released, then return `Value`.
    Gate(Arc<Notify>, Value),
    /// Block until cancelled and record that cancellation arrived.
    AwaitCancel(Arc<AtomicBool>),
}

#[derive(Default)]
struct ScriptedAgent {
    behaviors: Mutex<VecDeque<Behavior>>,
    requests: Mutex<Vec<AgentStepRequest>>,
    decisions: Mutex<Vec<(String, bool)>>,
}

impl ScriptedAgent {
    fn new(behaviors: Vec<Behavior>) -> Arc<Self> {
        Arc::new(Self {
            behaviors: Mutex::new(behaviors.into()),
            ..Self::default()
        })
    }

    fn requests(&self) -> Vec<AgentStepRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentStepExecutor for ScriptedAgent {
    async fn execute(
        &self,
        request: AgentStepRequest,
        mut context: StepContext,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        context.signal(StepSignal::Delegated(DelegatedRunRef {
            session_id: Some(format!("session-{}", request.attempt)),
            agent_id: Some("agent".into()),
            agent_run_id: Some(format!("run-{}", request.attempt)),
        }));
        self.requests.lock().unwrap().push(request);
        let behavior = self
            .behaviors
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted behavior");
        let value = match behavior {
            Behavior::Return(result) => result?,
            Behavior::AskPermission(value) => {
                context.signal(StepSignal::PermissionRequested {
                    permission_id: "perm-1".into(),
                    tool_name: "fs.edit".into(),
                });
                let resolution = context.permissions.recv().await.expect("decision");
                self.decisions.lock().unwrap().push((
                    resolution.permission_id,
                    matches!(resolution.decision, PermissionDecision::Approved),
                ));
                value
            }
            Behavior::Gate(gate, value) => {
                gate.notified().await;
                value
            }
            Behavior::AwaitCancel(flag) => {
                context.cancellation.cancelled().await;
                flag.store(true, Ordering::SeqCst);
                return Err(AgentExecutionError::new("cancelled", "cancelled"));
            }
        };
        Ok(AgentStepOutput { value })
    }
}

fn report(status: &str) -> Value {
    json!({ "summary": "done", "status": status, "artifacts": [], "claimsToVerify": [] })
}

fn ok(value: Value) -> Behavior {
    Behavior::Return(Ok(value))
}

fn compiled(definition: OrchestrationDefinition) -> Arc<CompiledOrchestration> {
    Arc::new(compile(definition).expect("definition compiles"))
}

fn runner(agent: Arc<ScriptedAgent>) -> OrchestrationRunner {
    OrchestrationRunner::new(compiled(default_orchestration_definition()), agent)
        .with_available_tools(["fs.read".to_string(), "fs.edit".to_string()])
}

fn input() -> Value {
    json!({ "request": "do the work" })
}

fn run_id(value: &str) -> OrchestrationRunId {
    OrchestrationRunId::from(value)
}

fn node(value: &str) -> OrchestrationNodeId {
    OrchestrationNodeId::from(value)
}

async fn wait_until(
    watch: &mut watch::Receiver<OrchestrationRunState>,
    predicate: impl Fn(&OrchestrationRunState) -> bool,
) {
    tokio::time::timeout(
        Duration::from_secs(5),
        watch.wait_for(|state| predicate(state)),
    )
    .await
    .expect("state reached in time")
    .expect("run still alive");
}

fn event_kinds(output: &OrchestrationRunOutput) -> Vec<&OrchestrationEvent> {
    output
        .events
        .iter()
        .map(|envelope| &envelope.event)
        .collect()
}

// ---------------------------------------------------------------------------
// Flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_flow_executes_input_agent_verify_and_output() {
    let agent = ScriptedAgent::new(vec![ok(report("completed"))]);
    let output = runner(agent.clone())
        .run(run_id("test-run"), input(), CancellationToken::new())
        .await
        .expect("run succeeds");

    assert_eq!(output.state.status, OrchestrationStatus::Completed);
    assert_eq!(output.result.status, OrchestrationOutcome::Completed);
    assert_eq!(output.result.output, Some(report("completed")));
    assert_eq!(agent.requests().len(), 1);
    assert_eq!(
        agent.requests()[0].tools,
        vec!["fs.edit".to_string(), "fs.read".to_string()],
        "inherit resolves to the host's explicit tool list"
    );

    // Every agent attempt is correlated to its node and delegated run.
    let correlations = output.agent_correlations();
    assert_eq!(correlations.len(), 1);
    assert_eq!(correlations[0].node_id, node("execute"));
    assert_eq!(
        correlations[0].delegated.agent_run_id.as_deref(),
        Some("run-1")
    );

    // The verify step records evidence and a structured report.
    let verify = &output.state.steps[&node("verify")];
    assert_eq!(verify.output.as_ref().unwrap()["passed"], json!(true));
    assert!(verify.attempts[0].details.evidence.iter().all(|e| e.passed));

    // Sequences are gap-free and completion is the last event.
    let sequences: Vec<_> = output.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, (1..=sequences.len() as u64).collect::<Vec<_>>());
    assert_eq!(
        event_kinds(&output).last(),
        Some(&&OrchestrationEvent::RunCompleted)
    );
}

#[tokio::test]
async fn invalid_agent_output_is_retried_with_the_validation_errors_as_feedback() {
    let agent = ScriptedAgent::new(vec![
        ok(json!({ "status": "completed" })),
        ok(report("completed")),
    ]);
    let output = runner(agent.clone())
        .run(run_id("retry-run"), input(), CancellationToken::new())
        .await
        .expect("retry succeeds");

    assert_eq!(output.state.status, OrchestrationStatus::Completed);
    let requests = agent.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].feedback.is_empty());
    assert_eq!(requests[1].feedback[0].code, "invalid_structured_output");
    assert!(requests[1].feedback[0].message.contains("summary"));
    assert_eq!(output.state.steps[&node("execute")].attempts.len(), 2);
}

#[tokio::test]
async fn retryable_executor_failure_is_retried() {
    let agent = ScriptedAgent::new(vec![
        Behavior::Return(Err(AgentExecutionError::retryable(
            "BACKEND_ERROR",
            "Timeout",
            RetryReason::BackendTimeout,
        ))),
        ok(report("completed")),
    ]);
    let output = runner(agent.clone())
        .run(run_id("backend-retry"), input(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Completed);
    assert_eq!(agent.requests().len(), 2);
    assert_eq!(
        output.agent_correlations().len(),
        2,
        "failed attempt correlated too"
    );
}

#[tokio::test]
async fn verification_failure_reruns_the_agent_with_the_issues() {
    let agent = ScriptedAgent::new(vec![ok(report("blocked")), ok(report("completed"))]);
    let output = runner(agent.clone())
        .run(run_id("verify-retry"), input(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(output.state.status, OrchestrationStatus::Completed);
    let requests = agent.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].feedback[0].code, "verification_failed");
    assert!(requests[1].feedback[0].message.contains("blocked"));
    let verify = &output.state.steps[&node("verify")];
    assert_eq!(verify.attempts.len(), 2);
    assert!(!verify.attempts[0].details.evidence.iter().all(|e| e.passed));
}

#[tokio::test]
async fn persistent_verification_failure_fails_the_run() {
    let agent = ScriptedAgent::new(vec![ok(report("blocked")), ok(report("failed"))]);
    let output = runner(agent)
        .run(run_id("verify-fail"), input(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.result.status, OrchestrationOutcome::Failed);
    assert_eq!(output.result.error.unwrap().code, "verification_failed");
}

#[tokio::test]
async fn unresolvable_artifacts_fail_verification() {
    struct Missing;
    #[async_trait]
    impl ArtifactResolver for Missing {
        async fn resolve(&self, _: &str, reference: &str) -> Result<(), String> {
            Err(format!("{reference} does not exist"))
        }
    }
    let claimed = json!({
        "summary": "wrote it", "status": "completed", "claimsToVerify": [],
        "artifacts": [{ "kind": "file", "reference": "src/reset.rs" }]
    });
    let agent = ScriptedAgent::new(vec![ok(claimed.clone()), ok(claimed)]);
    let output = runner(agent)
        .with_artifacts(Arc::new(Missing))
        .run(run_id("artifacts"), input(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.result.status, OrchestrationOutcome::Failed);
    assert!(output
        .result
        .error
        .unwrap()
        .message
        .contains("src/reset.rs"));
}

#[tokio::test]
async fn invalid_input_fails_before_agent_execution() {
    let agent = ScriptedAgent::new(Vec::new());
    let output = runner(agent.clone())
        .run(
            run_id("invalid-input"),
            json!({ "request": "" }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Failed);
    assert_eq!(output.result.error.unwrap().code, "invalid_input");
    assert!(agent.requests().is_empty());
}

// ---------------------------------------------------------------------------
// Tool scope
// ---------------------------------------------------------------------------

fn with_tool_scope(scope: ToolScope) -> OrchestrationDefinition {
    let mut definition = default_orchestration_definition();
    for node in &mut definition.nodes {
        if let OrchestrationNodeKind::Agent(config) = &mut node.kind {
            config.tools = scope.clone();
        }
    }
    definition
}

#[tokio::test]
async fn unknown_allow_listed_tools_are_rejected_before_any_side_effect() {
    let agent = ScriptedAgent::new(Vec::new());
    let error = OrchestrationRunner::new(
        compiled(with_tool_scope(ToolScope::AllowList(vec![
            "shell.exec".into()
        ]))),
        agent.clone(),
    )
    .with_available_tools(["fs.read".to_string()])
    .start(run_id("unknown-tool"), input())
    .err()
    .expect("rejected");
    assert!(
        matches!(error, OrchestrationRuntimeError::UnknownTools(tools) if tools == ["shell.exec"])
    );
    assert!(agent.requests().is_empty());
}

#[tokio::test]
async fn allow_lists_are_passed_through_and_inherit_without_a_host_list_fails_closed() {
    let agent = ScriptedAgent::new(vec![ok(report("completed"))]);
    OrchestrationRunner::new(
        compiled(with_tool_scope(ToolScope::AllowList(
            vec!["fs.read".into()],
        ))),
        agent.clone(),
    )
    .run(run_id("allow"), input(), CancellationToken::new())
    .await
    .unwrap();
    assert_eq!(agent.requests()[0].tools, vec!["fs.read".to_string()]);

    let agent = ScriptedAgent::new(Vec::new());
    let output =
        OrchestrationRunner::new(compiled(default_orchestration_definition()), agent.clone())
            .run(run_id("inherit"), input(), CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(output.result.error.unwrap().code, "tool_scope_unresolved");
    assert!(agent.requests().is_empty());
}

// ---------------------------------------------------------------------------
// Control: permissions, pause/resume, cancellation, budgets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn permission_requests_are_surfaced_and_resolved_through_the_handle() {
    let agent = ScriptedAgent::new(vec![Behavior::AskPermission(report("completed"))]);
    let mut handle = runner(agent.clone())
        .start(run_id("perm"), input())
        .unwrap();
    let mut updates = handle.subscribe();
    let mut watch = handle.watch();

    wait_until(&mut watch, |state| {
        state.status == OrchestrationStatus::WaitingForPermission
    })
    .await;
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.steps[&node("execute")].pending_permissions,
        vec!["perm-1".to_string()]
    );
    assert!(handle
        .resolve_permission("perm-other", PermissionDecision::Approved)
        .await
        .is_err());
    handle
        .resolve_permission("perm-1", PermissionDecision::Denied)
        .await
        .unwrap();

    let output = handle.wait().await.unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Completed);
    assert_eq!(
        *agent.decisions.lock().unwrap(),
        vec![("perm-1".to_string(), false)]
    );
    let mut saw_request = false;
    while let Ok(update) = updates.try_recv() {
        if let OrchestrationUpdate::Event(envelope) = update {
            saw_request |= matches!(
                envelope.event,
                OrchestrationEvent::PermissionRequested { ref permission_id, .. } if permission_id == "perm-1"
            );
        }
    }
    assert!(saw_request);
}

#[tokio::test]
async fn pause_holds_the_next_step_until_resume() {
    let gate = Arc::new(Notify::new());
    let agent = ScriptedAgent::new(vec![Behavior::Gate(gate.clone(), report("completed"))]);
    let handle = runner(agent).start(run_id("pause"), input()).unwrap();
    let mut watch = handle.watch();

    wait_until(&mut watch, |state| {
        state.steps[&node("execute")].status == StepStatus::Running
    })
    .await;
    handle.pause().await.unwrap();
    assert!(handle.pause().await.is_err(), "double pause is rejected");
    gate.notify_one();

    wait_until(&mut watch, |state| {
        state.status == OrchestrationStatus::Paused
    })
    .await;
    let paused = handle.snapshot();
    assert_eq!(paused.steps[&node("execute")].status, StepStatus::Succeeded);
    assert_eq!(paused.steps[&node("verify")].status, StepStatus::Ready);
    assert!(paused.steps[&node("verify")].attempts.is_empty());

    handle.resume().await.unwrap();
    let output = handle.wait().await.unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Completed);
}

#[tokio::test]
async fn cancellation_propagates_to_the_delegated_attempt() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let agent = ScriptedAgent::new(vec![Behavior::AwaitCancel(cancelled.clone())]);
    let handle = runner(agent).start(run_id("cancel"), input()).unwrap();
    let mut watch = handle.watch();
    wait_until(&mut watch, |state| {
        state.steps[&node("execute")].status == StepStatus::Running
    })
    .await;
    handle.cancel();
    let output = handle.wait().await.unwrap();

    assert_eq!(output.result.status, OrchestrationOutcome::Cancelled);
    assert_eq!(
        output.state.steps[&node("execute")].status,
        StepStatus::Cancelled
    );
    assert_eq!(
        output.state.steps[&node("verify")].status,
        StepStatus::Skipped
    );
    // The attempt's own token fired, so its delegated session was told to stop.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_before_start_prevents_agent_execution() {
    let agent = ScriptedAgent::new(Vec::new());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let output = runner(agent.clone())
        .run(run_id("cancelled"), input(), cancellation)
        .await
        .unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Cancelled);
    assert!(agent.requests().is_empty());
}

#[tokio::test]
async fn elapsed_budget_aborts_a_hanging_step() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let agent = ScriptedAgent::new(vec![Behavior::AwaitCancel(cancelled.clone())]);
    let mut definition = default_orchestration_definition();
    definition.policies.max_elapsed_ms = Some(50);
    let output = OrchestrationRunner::new(compiled(definition), agent)
        .with_available_tools(Vec::<String>::new())
        .run(run_id("elapsed"), input(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        output.result.error.unwrap().code,
        error_codes::BUDGET_EXHAUSTED
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(cancelled.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// Durability and restoration
// ---------------------------------------------------------------------------

struct FailingStore;

#[async_trait]
impl OrchestrationStore for FailingStore {
    async fn commit(
        &self,
        _: &[OrchestrationEventEnvelope],
        _: &OrchestrationSnapshot,
    ) -> Result<(), OrchestrationStoreError> {
        Err(std::io::Error::other("disk full").into())
    }
    async fn load_snapshot(
        &self,
        _: &OrchestrationRunId,
    ) -> Result<Option<OrchestrationSnapshot>, OrchestrationStoreError> {
        Ok(None)
    }
    async fn load_events(
        &self,
        _: &OrchestrationRunId,
    ) -> Result<Vec<OrchestrationEventEnvelope>, OrchestrationStoreError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn persistence_failure_halts_before_anything_is_published_or_executed() {
    let agent = ScriptedAgent::new(Vec::new());
    let mut handle = runner(agent.clone())
        .with_store(Arc::new(FailingStore))
        .start(run_id("disk-full"), input())
        .unwrap();
    let mut updates = handle.subscribe();
    let error = handle.wait().await.expect_err("fails closed");
    assert!(matches!(error, OrchestrationRuntimeError::Persistence(_)));
    assert!(updates.try_recv().is_err(), "nothing published");
    assert!(agent.requests().is_empty());
}

#[tokio::test]
async fn completed_runs_are_durable_and_completion_is_committed() {
    let store = Arc::new(InMemoryOrchestrationStore::new());
    let agent = ScriptedAgent::new(vec![ok(report("completed"))]);
    let output = runner(agent)
        .with_store(store.clone())
        .run(run_id("durable"), input(), CancellationToken::new())
        .await
        .unwrap();

    let events = store.load_events(&run_id("durable")).await.unwrap();
    assert_eq!(events, output.events);
    let snapshot = store
        .load_snapshot(&run_id("durable"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.state, output.state);
    assert_eq!(snapshot.last_sequence, events.len() as u64);
    assert_eq!(
        snapshot.result.unwrap().status,
        OrchestrationOutcome::Completed
    );
}

/// Commit a state built directly with the reducer, as if a previous process
/// had stopped right there.
async fn seed(
    store: &dyn OrchestrationStore,
    compiled: &CompiledOrchestration,
    steps: impl FnOnce(&mut OrchestrationRunState),
) {
    let mut state = OrchestrationRunState::new(run_id("crashed"), compiled);
    apply(
        compiled,
        &mut state,
        OrchestrationCommand::Start { input: input() },
    )
    .unwrap();
    steps(&mut state);
    store
        .commit(
            &[],
            &OrchestrationSnapshot {
                state,
                last_sequence: 0,
                elapsed_ms: 0,
                result: None,
            },
        )
        .await
        .unwrap();
}

fn step(
    compiled: &CompiledOrchestration,
    state: &mut OrchestrationRunState,
    name: &str,
    output: Option<Value>,
) {
    apply(
        compiled,
        state,
        OrchestrationCommand::StepAdmitted {
            node_id: node(name),
            attempt: 1,
        },
    )
    .unwrap();
    if let Some(output) = output {
        apply(
            compiled,
            state,
            OrchestrationCommand::StepSucceeded {
                node_id: node(name),
                attempt: 1,
                output,
                details: AttemptDetails::default(),
            },
        )
        .unwrap();
    }
}

#[tokio::test]
async fn resume_does_not_repeat_completed_steps() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileOrchestrationStore::new(dir.path()));
    let compiled = compiled(default_orchestration_definition());
    seed(store.as_ref(), &compiled, |state| {
        step(&compiled, state, "input", Some(input()));
        step(&compiled, state, "execute", Some(report("completed")));
    })
    .await;

    let agent = ScriptedAgent::new(Vec::new());
    let output = OrchestrationRunner::new(compiled.clone(), agent.clone())
        .with_store(store.clone())
        .resume(run_id("crashed"))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();

    assert_eq!(output.result.status, OrchestrationOutcome::Completed);
    assert!(agent.requests().is_empty(), "execute is not re-run");
    assert_eq!(output.state.steps[&node("execute")].attempts.len(), 1);
    assert_eq!(output.events[0].event, OrchestrationEvent::RunRestored);
    assert_eq!(output.events[0].sequence, 1);

    // Finished runs cannot be resumed again.
    let error = OrchestrationRunner::new(compiled, agent)
        .with_store(store)
        .resume(run_id("crashed"))
        .await
        .err()
        .expect("finished");
    assert!(matches!(error, OrchestrationRuntimeError::RunFinished(_)));
}

#[tokio::test]
async fn resume_fails_closed_on_an_interrupted_agent_attempt() {
    let store = Arc::new(InMemoryOrchestrationStore::new());
    let compiled = compiled(default_orchestration_definition());
    seed(store.as_ref(), &compiled, |state| {
        step(&compiled, state, "input", Some(input()));
        step(&compiled, state, "execute", None);
    })
    .await;

    let agent = ScriptedAgent::new(Vec::new());
    let output = OrchestrationRunner::new(compiled, agent.clone())
        .with_store(store)
        .resume(run_id("crashed"))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(
        output.result.error.unwrap().code,
        error_codes::INDETERMINATE_ATTEMPT
    );
    assert!(agent.requests().is_empty());
}

#[tokio::test]
async fn resume_rejects_an_edited_definition() {
    let store = Arc::new(InMemoryOrchestrationStore::new());
    let original = compiled(default_orchestration_definition());
    seed(store.as_ref(), &original, |_| {}).await;
    let mut edited = default_orchestration_definition();
    edited.name = "Edited in place".into();
    let error = OrchestrationRunner::new(compiled(edited), ScriptedAgent::new(Vec::new()))
        .with_store(store)
        .resume(run_id("crashed"))
        .await
        .err()
        .expect("rejected");
    assert!(matches!(error, OrchestrationRuntimeError::Restore(_)));
}

#[tokio::test]
async fn file_store_rejects_path_like_run_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileOrchestrationStore::new(dir.path());
    assert!(matches!(
        store.load_snapshot(&run_id("../escape")).await,
        Err(OrchestrationStoreError::InvalidRunId(_))
    ));
}

// ---------------------------------------------------------------------------
// End to end through a real session
// ---------------------------------------------------------------------------

mod isolated_session {
    use harness_protocol::{
        backend::{ExecutionEvent, ExecutionResult},
        ids::{RequestId, SessionId},
        usage::{Cost, ModelUsage},
    };

    use super::*;
    use crate::{
        session_agent_executor::IsolatedSessionAgentExecutor,
        session_runtime::SessionRuntime,
        testing::{FakeBackend, FakeToolRegistry},
        traits::EventSink,
        workspace::FakeWorkspace,
    };

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _: harness_protocol::events::AgentEventEnvelope) {}
    }

    #[tokio::test]
    async fn default_workflow_runs_through_an_isolated_session() {
        let request_id = RequestId::new();
        let result = ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        };
        let backend = FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: report("completed").to_string(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: result.clone(),
                },
            ])
            .with_result(result);
        let parent = Arc::new(SessionRuntime::new(
            SessionId::new(),
            Arc::new(backend),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
        ));

        let mut handle = OrchestrationRunner::new(
            compiled(default_orchestration_definition()),
            Arc::new(IsolatedSessionAgentExecutor::new(parent.clone())),
        )
        .with_available_tools(Vec::<String>::new())
        .start(run_id("e2e"), input())
        .unwrap();
        let mut updates = handle.subscribe();
        let output = handle.wait().await.unwrap();

        assert_eq!(
            output.result.status,
            OrchestrationOutcome::Completed,
            "{:?}",
            output.result
        );
        assert_eq!(output.result.output, Some(report("completed")));

        let correlation = &output.agent_correlations()[0];
        assert_eq!(correlation.node_id, node("execute"));
        let delegated_session = correlation.delegated.session_id.clone().unwrap();
        assert_ne!(delegated_session, parent.session_id.to_string(), "isolated");
        assert!(correlation.delegated.agent_run_id.is_some());

        let mut agent_events = 0;
        while let Ok(update) = updates.try_recv() {
            if let OrchestrationUpdate::Agent(event) = update {
                assert_eq!(event.correlation.node_id, node("execute"));
                assert_eq!(event.envelope.session_id.to_string(), delegated_session);
                agent_events += 1;
            }
        }
        assert!(agent_events > 0, "delegated agent events are republished");
    }
}

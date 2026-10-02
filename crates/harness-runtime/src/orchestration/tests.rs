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
    ExhaustUsage,
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
            Behavior::ExhaustUsage => {
                context.signal(StepSignal::Usage(
                    harness_core::orchestration::UsageSummary {
                        model_requests: 2,
                        ..Default::default()
                    },
                ));
                context.cancellation.cancelled().await;
                return Err(AgentExecutionError::new("cancelled", "budget cancelled"));
            }
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
    use harness_core::orchestration::{SchemaReference, StructuredOutputMode, VerificationCheck};
    use harness_protocol::{
        backend::{
            BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent,
            ExecutionRequest, ExecutionResult,
        },
        ids::{RequestId, SessionId},
        usage::{Cost, ModelUsage},
    };
    use tokio::sync::broadcast;

    use super::*;
    use crate::{
        session_agent_executor::IsolatedSessionAgentExecutor,
        session_runtime::SessionRuntime,
        testing::{FakeBackend, FakeToolRegistry},
        traits::{EventSink, ExecutionBackend},
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

    /// Keeps every request it is sent, and otherwise behaves as the wrapped
    /// backend -- which, like the OpenAI-compatible integration, advertises
    /// native structured output whatever model sits behind it.
    struct RecordingBackend {
        inner: FakeBackend,
        requests: Arc<Mutex<Vec<ExecutionRequest>>>,
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

    struct QueueBackend {
        replies: Mutex<VecDeque<Value>>,
        requests: Arc<Mutex<Vec<ExecutionRequest>>>,
    }
    #[async_trait]
    impl ExecutionBackend for QueueBackend {
        fn descriptor(&self) -> BackendDescriptor {
            FakeBackend::new().descriptor()
        }
        fn capabilities(&self) -> BackendCapabilities {
            FakeBackend::new().capabilities()
        }
        async fn execute(
            &self,
            request: ExecutionRequest,
            sink: broadcast::Sender<ExecutionEvent>,
            cancel: CancellationToken,
        ) -> Result<ExecutionResult, ExecutionError> {
            self.requests.lock().unwrap().push(request.clone());
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected model call");
            let request_id = request.request_id;
            let result = ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            };
            FakeBackend::new()
                .with_events(vec![
                    ExecutionEvent::TextDelta {
                        request_id,
                        delta: reply.to_string(),
                    },
                    ExecutionEvent::Completed {
                        request_id,
                        result: result.clone(),
                    },
                ])
                .with_result(result)
                .execute(request, sink, cancel)
                .await
        }
    }

    #[tokio::test]
    async fn task_queue_repairs_and_resumes_without_repeating_accepted_tasks() {
        use harness_core::{
            behavior::{ProfileRef, ProfileRegistry},
            orchestration::{InputBinding, OutputBinding, TaskQueueConfig},
        };
        let built = json!({"status":"complete","summary":"implemented"});
        let reviewed = |id: &str| json!({"status":"complete","summary":"inspected actual code","criteria":[{"id":id,"evidence":"file.rs:12 implements behavior"}]});
        let requests = Arc::new(Mutex::new(Vec::new()));
        let parent = Arc::new(SessionRuntime::new(SessionId::new(), Arc::new(QueueBackend {
            requests: requests.clone(),
            replies: Mutex::new(vec![built.clone(), reviewed("C1"), built.clone(), json!({"status":"needs_repair","summary":"missing edge case","criteria":[]}), json!({"status":"blocked_user","summary":"access denied"}), built, reviewed("C2")].into()),
        }), Arc::new(FakeToolRegistry::new()), Arc::new(FakeWorkspace::new()), Arc::new(NoopSink)));
        let mut definition = default_orchestration_definition();
        definition.input_schema = None;
        definition.output_contract.schema = SchemaReference::Inline {
            name: "tasks".into(),
            schema: json!({"type":"object"}),
        };
        for n in &mut definition.nodes {
            if let OrchestrationNodeKind::Agent(config) = &mut n.kind {
                config.task_queue = Some(TaskQueueConfig {
                    plan_pointer: "/plan".into(),
                    review_profile: ProfileRef {
                        id: "rusty.default".into(),
                        revision: None,
                    },
                    review_instructions: "Inspect the task criteria.".into(),
                    max_repairs: 2,
                });
                config.structured_output = StructuredOutputMode::HostValidated;
                n.output_schema = Some(definition.output_contract.schema.clone());
                n.input_bindings.push(InputBinding {
                    target: "plan".into(),
                    source: OutputBinding::RunInput {
                        pointer: "/plan".into(),
                    },
                });
            }
            if let OrchestrationNodeKind::Verify(config) = &mut n.kind {
                config.checks = vec![VerificationCheck::RequiredStatus {
                    pointer: "/report/status".into(),
                    equals: "implemented".into(),
                }];
            }
        }
        let runner = OrchestrationRunner::new(
            compiled(definition),
            Arc::new(
                IsolatedSessionAgentExecutor::new(parent)
                    .with_profiles(Arc::new(ProfileRegistry::new())),
            ),
        )
        .with_available_tools(Vec::<String>::new());
        let input = json!({"request":"two changes","plan":{"status":"ready","summary":"two tasks","requirements":[{"id":"R1","text":"both changes","criteria":[{"id":"C1","text":"first"},{"id":"C2","text":"second"}]}],"tasks":[{"id":"T1","instructions":"first","requirement_ids":["R1"],"criterion_ids":["C1"],"depends_on":[]},{"id":"T2","instructions":"second","requirement_ids":["R1"],"criterion_ids":["C2"],"depends_on":["T1"]}]}});
        let failed = runner
            .start(run_id("queue-first"), input)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(
            failed.state.status,
            OrchestrationStatus::Failed,
            "{:?}",
            failed.result
        );
        let checkpoint = failed.state.steps[&node("execute")]
            .checkpoint
            .as_ref()
            .unwrap();
        assert_eq!(checkpoint["completed"].as_array().unwrap().len(), 1);
        assert_eq!(checkpoint["current"]["task_id"], "T2");
        let saved = serde_json::from_value(serde_json::to_value(&failed.state).unwrap()).unwrap();
        let continued = runner
            .retry_failed(
                saved,
                run_id("queue-resumed"),
                "environment fixed; continue".into(),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(
            continued.state.status,
            OrchestrationStatus::Completed,
            "{:?}",
            continued.result
        );
        assert_eq!(
            continued.result.output.as_ref().unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let sent = requests.lock().unwrap();
        assert_eq!(
            sent.len(),
            7,
            "one task review, local repair, then resume only second task"
        );
        let session_ids: std::collections::HashSet<_> = sent.iter().map(|r| r.run_id).collect();
        assert_eq!(
            session_ids.len(),
            7,
            "every builder/reviewer has a fresh run"
        );
        assert!(prompt_of(&sent[4]).contains("missing edge case"));
        assert!(prompt_of(&sent[5]).contains("access denied"));
    }

    /// Runs the default workflow with its agent step in `mode`, against a
    /// backend that advertises native structured output, and returns the
    /// model requests the step made.
    async fn requests_sent(mode: StructuredOutputMode) -> Vec<ExecutionRequest> {
        let request_id = RequestId::new();
        let result = ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        };
        let inner = FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: if mode == StructuredOutputMode::Text {
                        "A plain message with {invalid JSON}.".into()
                    } else {
                        report("completed").to_string()
                    },
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: result.clone(),
                },
            ])
            .with_result(result);
        assert!(
            inner.capabilities().structured_output,
            "this test is about backends that advertise native structured output"
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let parent = Arc::new(SessionRuntime::new(
            SessionId::new(),
            Arc::new(RecordingBackend {
                inner,
                requests: requests.clone(),
            }),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
        ));

        let mut definition = default_orchestration_definition();
        for node in &mut definition.nodes {
            if let OrchestrationNodeKind::Agent(config) = &mut node.kind {
                config.structured_output = mode;
                if mode == StructuredOutputMode::Text {
                    node.output_schema = None;
                }
            }
            if mode == StructuredOutputMode::Text {
                if let OrchestrationNodeKind::Verify(config) = &mut node.kind {
                    config.checks.clear();
                    config.checks.push(VerificationCheck::ArtifactExists {
                        pointer: "/report".into(),
                    });
                }
            }
        }
        if mode == StructuredOutputMode::Text {
            definition.output_contract.schema = SchemaReference::Inline {
                name: "text".into(),
                schema: json!({"type": "string"}),
            };
        }
        let handle = OrchestrationRunner::new(
            compiled(definition),
            Arc::new(IsolatedSessionAgentExecutor::new(parent)),
        )
        .with_available_tools(Vec::<String>::new())
        .start(run_id("modes"), input())
        .unwrap();
        let output = handle.wait().await.unwrap();
        assert_eq!(
            output.result.status,
            OrchestrationOutcome::Completed,
            "{:?}",
            output.result
        );
        let sent = requests.lock().unwrap().clone();
        assert!(!sent.is_empty(), "the step made a model request");
        sent
    }

    fn prompt_of(request: &ExecutionRequest) -> String {
        serde_json::to_string(&request.messages).unwrap()
    }

    /// A native schema rides on every request of a step, tool-calling turns
    /// included. Some models reject that outright ("function calling with a
    /// response mime type is unsupported" on Gemini), and some providers
    /// reject a schema they judge too large, so a workflow must be able to
    /// keep the schema out of the request and have the host validate instead.
    #[tokio::test]
    async fn host_validated_steps_never_send_a_native_schema() {
        for request in requests_sent(StructuredOutputMode::HostValidated).await {
            assert_eq!(request.params.response_format, None);
            let prompt = prompt_of(&request);
            assert!(
                prompt.contains("plain Markdown, not JSON") && !prompt.contains("JSON Schema"),
                "the reply layout travels in the prompt instead: {prompt}"
            );
        }
    }

    #[tokio::test]
    async fn text_steps_never_send_or_request_a_schema_even_if_supported() {
        for request in requests_sent(StructuredOutputMode::Text).await {
            assert_eq!(request.params.response_format, None);
            let prompt = prompt_of(&request);
            assert!(!prompt.contains("JSON Schema"));
            assert!(prompt.contains("ordinary text or Markdown"));
        }
    }

    /// Object schemas are answered in Markdown, so even a backend with
    /// native structured output is not asked for JSON.
    #[tokio::test]
    async fn fallback_steps_ask_for_markdown_even_when_the_backend_has_native_schemas() {
        for request in requests_sent(StructuredOutputMode::HostValidatedFallback).await {
            assert_eq!(request.params.response_format, None);
            assert!(prompt_of(&request).contains("plain Markdown, not JSON"));
        }
    }
}

#[tokio::test]
async fn explicit_retry_preserves_plan_and_restarts_only_failed_build() {
    use harness_core::orchestration::{InputBinding, OutputBinding};
    let mut definition = default_orchestration_definition();
    let mut plan = definition
        .nodes
        .iter()
        .find(|n| n.id == node("execute"))
        .unwrap()
        .clone();
    plan.id = node("plan");
    plan.name = "Plan".into();
    definition.nodes.push(plan);
    definition.edges[0].target = node("plan");
    let mut edge = definition.edges[0].clone();
    edge.id = "plan-build".into();
    edge.source = node("plan");
    edge.target = node("execute");
    definition.edges.push(edge);
    definition
        .nodes
        .iter_mut()
        .find(|n| n.id == node("execute"))
        .unwrap()
        .input_bindings
        .push(InputBinding {
            target: "plan".into(),
            source: OutputBinding::NodeOutput {
                node_id: node("plan"),
                pointer: String::new(),
            },
        });
    let agent = ScriptedAgent::new(vec![
        ok(report("completed")),
        Behavior::Return(Err(AgentExecutionError::new("backend", "HTTP 400"))),
        ok(report("completed")),
    ]);
    let runner = OrchestrationRunner::new(compiled(definition), agent.clone())
        .with_available_tools(["fs.read".into(), "fs.edit".into()]);
    let failed = runner
        .run(run_id("first"), input(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(failed.state.failed_step, Some(node("execute")));
    assert_eq!(failed.state.status, OrchestrationStatus::Failed);
    let saved_plan = failed.state.steps[&node("plan")].clone();
    // Round-trip through chat persistence before continuation.
    let checkpoint = serde_json::from_value(serde_json::to_value(&failed.state).unwrap()).unwrap();
    let continued = runner
        .retry_failed(checkpoint, run_id("continued"), "continue please".into())
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(continued.state.status, OrchestrationStatus::Completed);
    assert_eq!(continued.state.steps[&node("plan")], saved_plan);
    assert_eq!(continued.state.input, Some(input()));
    let requests = agent.requests();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.node_id.clone())
            .collect::<Vec<_>>(),
        vec![node("plan"), node("execute"), node("execute")]
    );
    assert_eq!(requests[2].attempt, 2);
    assert_eq!(requests[1].input, requests[2].input);
    assert!(requests[2]
        .feedback
        .iter()
        .any(|f| f.message.contains("continue please")));
}

#[tokio::test]
async fn explicit_retry_rejects_changed_definitions_and_successful_runs() {
    let agent = ScriptedAgent::new(vec![Behavior::Return(Err(AgentExecutionError::new(
        "backend", "failed",
    )))]);
    let original = runner(agent.clone());
    let failed = original
        .run(run_id("first"), input(), CancellationToken::new())
        .await
        .unwrap();
    let mut changed = default_orchestration_definition();
    changed.nodes[1].name = "different definition".into();
    let changed_runner = OrchestrationRunner::new(compiled(changed), agent.clone());
    assert!(changed_runner
        .retry_failed(failed.state.clone(), run_id("next"), "continue".into())
        .is_err());
    let mut completed = failed.state;
    completed.status = OrchestrationStatus::Completed;
    assert!(original
        .retry_failed(completed, run_id("next"), "continue".into())
        .is_err());
    assert_eq!(agent.requests().len(), 1);
}

#[tokio::test]
async fn live_usage_budget_stops_a_long_running_queue_step() {
    let mut definition = default_orchestration_definition();
    definition.policies.max_model_requests = Some(2);
    let runner = OrchestrationRunner::new(
        compiled(definition),
        ScriptedAgent::new(vec![Behavior::ExhaustUsage]),
    )
    .with_available_tools(Vec::<String>::new());
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        runner.start(run_id("live-budget"), input()).unwrap().wait(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.state.status, OrchestrationStatus::Failed);
    assert_eq!(
        output.result.error.unwrap().code,
        error_codes::BUDGET_EXHAUSTED
    );
}

#[test]
fn acceptance_rejects_missing_and_failed_requirements_but_not_renamed_ids() {
    let plan = json!({"requirements":[{"criteria":[{"id":"C1"},{"id":"C2"}]}]});
    let pass = json!({"id":"C1","status":"pass","evidence":"observed behavior"});
    let second = json!({"id":"C2","status":"pass","evidence":"regression test passed"});
    let coverage =
        |results: Value| super::steps::requirement_coverage(Some(&plan), Some(&results)).0;
    assert!(!coverage(json!([pass])));
    assert!(!coverage(json!([pass, pass])));
    assert!(!coverage(
        json!([pass,{"id":"C2","status":"fail","evidence":"defect"}])
    ));
    assert!(coverage(json!([pass, second])));
    // IDs are matched loosely, and results numbered another way answer in order.
    assert!(coverage(json!([
        {"id":"c-1","status":"pass","evidence":"observed behavior"},
        {"id":"C-2","status":"pass","evidence":"regression test passed"}
    ])));
    assert!(coverage(json!([
        {"id":"1","status":"pass","evidence":"observed behavior"},
        {"id":"2","status":"pass","evidence":"regression test passed"}
    ])));
    assert!(!coverage(json!([
        {"id":"1","status":"pass","evidence":"observed behavior"},
        {"id":"2","status":"fail","evidence":"defect"}
    ])));
}

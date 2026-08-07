//! M5: end-to-end tests for the `agent.spawn` tool — driven through the
//! *real* tool-call path (a scripted `ToolCallRequested` execution event),
//! not by constructing `AgentCommand::SpawnChild` directly, so these prove
//! the actual model-facing entry point works, not just its building blocks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendReference, ExecutionEvent, ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentStatus};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
use harness_protocol::ids::{
    AgentId, ConfigurationId, IntegrationId, RequestId, RunId, SessionId, ToolCallId, ToolId,
};
use harness_protocol::tools::{AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolPolicy};
use harness_protocol::usage::AgentBudget;
use harness_runtime::agent_runner::{AgentRunner, AgentTask};
use harness_runtime::agent_supervisor::AgentSupervisor;
use harness_runtime::cancellation::SessionCancellation;
use harness_runtime::integration::IntegrationRegistry;
use harness_runtime::scheduler::{Scheduler, SchedulerConfig};
use harness_runtime::session_runtime::LiveStateTable;
use harness_runtime::spawn_tool::{agent_spawn_tool_descriptor, AGENT_SPAWN_TOOL_NAME};
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<AgentEventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn send(&self, event: AgentEventEnvelope) {
        self.events.lock().expect("event lock poisoned").push(event);
    }
}

impl RecordingSink {
    fn events_for(&self, agent_id: AgentId) -> Vec<AgentEvent> {
        self.events
            .lock()
            .expect("event lock poisoned")
            .iter()
            .filter(|e| e.agent_id == agent_id)
            .map(|e| e.event.clone())
            .collect()
    }

    fn tool_completed_output(
        &self,
        agent_id: AgentId,
        call_id: ToolCallId,
    ) -> Option<serde_json::Value> {
        self.events_for(agent_id)
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::ToolCallCompleted {
                    call_id: id,
                    result,
                } if id == call_id => serde_json::from_str(&result.output_preview).ok(),
                _ => None,
            })
    }
}

/// Builds a root agent whose only registered tool is `agent.spawn`, gated
/// by `permission` (Allow for the happy-path tests, Ask for the permission
/// gating test), and `delegatable: false` (the M5 default — a spawned
/// child does not automatically get the ability to spawn further children).
///
/// Returns the agent with an `active_run` already set (status `Executing`)
/// rather than driven there via a real `StartRun` — this test suite cares
/// about what happens *after* a model requests the `agent.spawn` tool
/// call, not about faking a full realistic model turn first, and
/// `backend_event()` only accepts a `BackendEvent` whose `run_id` matches
/// `active_run`, so tests inject the tool call directly against this
/// pre-set run rather than racing a real (and, with `FakeBackend`, nearly
/// instant) `StartRun` completion that would clear `active_run` again
/// before the test gets a chance to act.
fn root_agent_with_spawn_tool(
    session_id: SessionId,
    agent_id: AgentId,
    backend: &dyn ExecutionBackend,
    permission: PermissionMode,
    budget: AgentBudget,
) -> (Agent, RunId) {
    let spawn_tool_id = ToolId::new();
    let mut tools = HashMap::new();
    tools.insert(
        spawn_tool_id,
        ToolCapability {
            descriptor: agent_spawn_tool_descriptor(spawn_tool_id),
            policy: ToolPolicy {
                permission,
                enabled: true,
            },
            delegatable: false,
        },
    );

    let mut agent = Agent::new(
        agent_id,
        session_id,
        None,
        0,
        "root".into(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: backend.descriptor(),
        },
        AgentCapabilities {
            tools: AgentToolset { tools },
            can_spawn_agents: true,
            max_child_depth: Some(4),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: true,
                can_search: true,
            },
            backend: BackendCapabilities::default(),
        },
        budget,
    );
    let run_id = RunId::new();
    agent.state.active_run = Some(run_id);
    agent.state.status = AgentStatus::Executing;
    (agent, run_id)
}

/// Spawns the standard test harness (supervisor + runner + mailbox) for one
/// root agent, returning the pieces a test needs to drive it.
#[allow(clippy::type_complexity)]
fn bootstrap(
    session_id: SessionId,
    root_id: AgentId,
    backend: Arc<dyn ExecutionBackend>,
    root: Agent,
) -> (
    tokio::sync::mpsc::Sender<AgentCommand>,
    Arc<RecordingSink>,
    AgentSupervisor,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let tools: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
    let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
    let sink = Arc::new(RecordingSink::default());
    let event_sink: Arc<dyn EventSink> = sink.clone();
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));
    let integrations = Arc::new(IntegrationRegistry::new());
    let cancellation = SessionCancellation::new();
    let root_cancel = cancellation.child_token();
    let supervisor = AgentSupervisor::new(session_id, cancellation);
    supervisor.register_agent_token(root_id, root_cancel.clone());

    let (task, commands) = AgentTask::new(root_id);
    let runner = AgentRunner::new(
        root,
        task,
        backend,
        tools,
        workspace,
        event_sink,
        root_cancel.clone(),
        LiveStateTable::default(),
        scheduler,
    )
    .with_supervision(supervisor.clone(), integrations);

    let runner_task = tokio::spawn(async move {
        let mut runner = runner;
        runner.run().await;
    });

    (commands, sink, supervisor, root_cancel, runner_task)
}

async fn send_spawn_tool_call(
    commands: &tokio::sync::mpsc::Sender<AgentCommand>,
    run_id: RunId,
    call_id: ToolCallId,
    arguments: serde_json::Value,
) {
    let request_id = RequestId::new();
    commands
        .send(AgentCommand::BackendEvent {
            run_id,
            event: ExecutionEvent::ToolCallRequested {
                request_id,
                call: ToolCall {
                    id: call_id,
                    name: AGENT_SPAWN_TOOL_NAME.to_string(),
                    arguments,
                },
            },
        })
        .await
        .expect("send ToolCallRequested");
}

async fn wait_for<F: Fn() -> bool>(predicate: F, timeout: Duration, what: &str) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::task::yield_now().await;
    }
}

/// The full happy path: a model calls `agent.spawn` with `mode: "await"`,
/// the child actually runs (via the shared `FakeBackend` script, since
/// `BackendPolicy::Inherit` is the default), and the tool call resolves
/// with the child's summary in its `ToolResult` — proving the tool
/// genuinely drives a working child, not just a spec construction.
#[tokio::test]
async fn spawn_tool_await_mode_runs_the_child_and_returns_its_summary() {
    let session_id = SessionId::new();
    let root_id = AgentId::new();
    let request_id = RequestId::new();
    let backend: Arc<dyn ExecutionBackend> = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "child-work".into(),
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: Default::default(),
                cost: Default::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let (root, run_id) = root_agent_with_spawn_tool(
        session_id,
        root_id,
        backend.as_ref(),
        PermissionMode::Allow,
        AgentBudget {
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    );
    let (commands, sink, _supervisor, root_cancel, runner_task) =
        bootstrap(session_id, root_id, backend, root);

    let call_id = ToolCallId::new();
    send_spawn_tool_call(
        &commands,
        run_id,
        call_id,
        serde_json::json!({ "task": "investigate the bug", "mode": "await" }),
    )
    .await;

    wait_for(
        || sink.tool_completed_output(root_id, call_id).is_some(),
        Duration::from_secs(5),
        "ToolCallCompleted for the spawn call",
    )
    .await;

    let output = sink
        .tool_completed_output(root_id, call_id)
        .expect("output present");
    assert_eq!(output["status"], "completed");
    assert!(output["child_agent_id"].is_string());
    assert!(
        output["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("end_turn"),
        "expected the child's real completion summary, got {output:?}"
    );

    // ChildAgentSpawned/ChildAgentCompleted must both be observable on the
    // parent, independent of the tool result — the same lineage a
    // Rust-orchestrated spawn produces (M5.4).
    let parent_events = sink.events_for(root_id);
    assert!(parent_events
        .iter()
        .any(|e| matches!(e, AgentEvent::ChildAgentSpawned { .. })));
    assert!(parent_events
        .iter()
        .any(|e| matches!(e, AgentEvent::ChildAgentCompleted { .. })));

    root_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

/// A budget escalation attempt (child budget looser than the parent's own)
/// must be rejected by the supervisor's existing validation — proving the
/// tool cannot bypass it — and reported back to the model as a readable
/// tool error, not a silent failure or a generic `ToolFailed`.
#[tokio::test]
async fn spawn_tool_cannot_escalate_past_the_parents_own_budget() {
    let session_id = SessionId::new();
    let root_id = AgentId::new();
    let request_id = RequestId::new();
    let backend: Arc<dyn ExecutionBackend> =
        Arc::new(FakeBackend::new().with_result(ExecutionResult {
            request_id,
            usage: Default::default(),
            cost: Default::default(),
            finish_reason: "end_turn".into(),
        }));
    let (root, run_id) = root_agent_with_spawn_tool(
        session_id,
        root_id,
        backend.as_ref(),
        PermissionMode::Allow,
        AgentBudget {
            max_requests: Some(5),
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    );
    let (commands, sink, _supervisor, root_cancel, runner_task) =
        bootstrap(session_id, root_id, backend, root);

    let call_id = ToolCallId::new();
    // Request a *looser* budget (20 > the parent's own 5) — must be rejected.
    send_spawn_tool_call(
        &commands,
        run_id,
        call_id,
        serde_json::json!({ "task": "go", "budget": { "max_requests": 20 } }),
    )
    .await;

    wait_for(
        || sink.tool_completed_output(root_id, call_id).is_some(),
        Duration::from_secs(5),
        "ToolCallCompleted for the rejected spawn call",
    )
    .await;

    let output = sink
        .tool_completed_output(root_id, call_id)
        .expect("output present");
    assert!(
        output.get("error").is_some(),
        "escalation must surface as a readable error, got {output:?}"
    );
    // No child should ever have been spawned.
    assert!(!sink
        .events_for(root_id)
        .iter()
        .any(|e| matches!(e, AgentEvent::ChildAgentSpawned { .. })));

    root_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

/// Permission gating (M5.6): with the spawn tool set to `Ask`, calling it
/// must produce a `PermissionRequested` event and *not* spawn a child until
/// approved — proving the tool goes through the exact same generic
/// permission machinery as any other tool, with no special bypass.
#[tokio::test]
async fn spawn_tool_respects_ask_permission_gating() {
    let session_id = SessionId::new();
    let root_id = AgentId::new();
    let request_id = RequestId::new();
    let backend: Arc<dyn ExecutionBackend> =
        Arc::new(FakeBackend::new().with_result(ExecutionResult {
            request_id,
            usage: Default::default(),
            cost: Default::default(),
            finish_reason: "end_turn".into(),
        }));
    let (root, run_id) = root_agent_with_spawn_tool(
        session_id,
        root_id,
        backend.as_ref(),
        PermissionMode::Ask,
        AgentBudget {
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    );
    let (commands, sink, _supervisor, root_cancel, runner_task) =
        bootstrap(session_id, root_id, backend, root);

    let call_id = ToolCallId::new();
    send_spawn_tool_call(
        &commands,
        run_id,
        call_id,
        serde_json::json!({ "task": "go" }),
    )
    .await;

    wait_for(
        || {
            sink.events_for(root_id)
                .iter()
                .any(|e| matches!(e, AgentEvent::PermissionRequested { .. }))
        },
        Duration::from_secs(5),
        "PermissionRequested for the spawn call",
    )
    .await;

    // Give the runner a few ticks to prove it does *not* spawn without approval.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !sink
            .events_for(root_id)
            .iter()
            .any(|e| matches!(e, AgentEvent::ChildAgentSpawned { .. })),
        "a child must never be spawned before the Ask permission is resolved"
    );

    root_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

/// Cancellation tree (M5.5): a `concurrent`-mode spawn's child token is a
/// descendant of the parent/session token, so cancelling the parent must
/// cancel the still-running child too — the tool-triggered path must not
/// create an orphan that survives its parent's shutdown.
#[tokio::test]
async fn cancelling_the_parent_cancels_a_concurrently_spawned_child() {
    let session_id = SessionId::new();
    let root_id = AgentId::new();
    let request_id = RequestId::new();
    // The child blocks until cancelled — if cancellation doesn't propagate,
    // this test hangs until its own timeout, which is exactly the failure
    // mode this test exists to catch.
    let backend: Arc<dyn ExecutionBackend> = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "working".into(),
            }])
            .blocking_until_cancelled(),
    );
    let (root, run_id) = root_agent_with_spawn_tool(
        session_id,
        root_id,
        backend.as_ref(),
        PermissionMode::Allow,
        AgentBudget {
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    );
    let (commands, sink, supervisor, root_cancel, runner_task) =
        bootstrap(session_id, root_id, backend, root);

    let call_id = ToolCallId::new();
    send_spawn_tool_call(
        &commands,
        run_id,
        call_id,
        serde_json::json!({ "task": "long running work", "mode": "concurrent" }),
    )
    .await;

    let child_id = {
        wait_for(
            || {
                sink.events_for(root_id)
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ChildAgentSpawned { .. }))
            },
            Duration::from_secs(5),
            "ChildAgentSpawned",
        )
        .await;
        sink.events_for(root_id)
            .into_iter()
            .find_map(|e| match e {
                AgentEvent::ChildAgentSpawned { agent_id } => Some(agent_id),
                _ => None,
            })
            .expect("child id")
    };

    // The child is still registered (still running, blocked on its own
    // cancellation) at this point.
    assert!(supervisor.child_commands(child_id).await.is_some());

    // Cancel the whole tree from the top, the way session-level shutdown does.
    root_cancel.cancel();

    // Once the child observes cancellation and its runner task exits, the
    // supervisor deregisters it (concurrent/detached children deregister
    // themselves — see `AgentSupervisor::spawn_child`'s `detached` branch).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if supervisor.child_commands(child_id).await.is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "spawned child was not deregistered after parent cancellation — cancellation did not propagate"
        );
        tokio::task::yield_now().await;
    }

    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

/// A [`FakeBackend`] wrapper that records the `model` field of every
/// [`ExecutionRequest`] it's asked to execute, so a test can prove a
/// `agent.spawn` `model` override actually reaches the backend call for the
/// *child's* run — not just that `build_spawn_spec` sets it on the spec
/// (already covered by `spawn_tool::tests::model_override_becomes_an_execution_params_override`
/// at the unit level).
struct ModelRecordingBackend {
    inner: FakeBackend,
    requested_models: Mutex<Vec<Option<String>>>,
}

impl ModelRecordingBackend {
    fn new(inner: FakeBackend) -> Self {
        Self {
            inner,
            requested_models: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ExecutionBackend for ModelRecordingBackend {
    fn descriptor(&self) -> harness_protocol::backend::BackendDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn execute(
        &self,
        request: harness_protocol::backend::ExecutionRequest,
        sink: tokio::sync::broadcast::Sender<ExecutionEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExecutionResult, harness_protocol::backend::ExecutionError> {
        self.requested_models
            .lock()
            .expect("requested_models lock poisoned")
            .push(request.params.model.clone());
        self.inner.execute(request, sink, cancel).await
    }
}

/// M5: a `model` override in `agent.spawn`'s arguments must reach the
/// spawned child's actual backend call — proving the whole chain (tool args
/// → `SpawnAgentSpec::execution_params` → `AgentState::execution_params` →
/// `ExecutionRequest.params.model`, the same M4-verified plumbing
/// `ConfigureExecution` already uses) works end to end through the real
/// tool-call path, not just at the spec-construction unit-test level.
#[tokio::test]
async fn spawn_tool_model_override_reaches_the_childs_backend_call() {
    let session_id = SessionId::new();
    let root_id = AgentId::new();
    let request_id = RequestId::new();
    let recording_backend = Arc::new(ModelRecordingBackend::new(FakeBackend::new().with_result(
        ExecutionResult {
            request_id,
            usage: Default::default(),
            cost: Default::default(),
            finish_reason: "end_turn".into(),
        },
    )));
    let backend: Arc<dyn ExecutionBackend> = recording_backend.clone();
    let (root, run_id) = root_agent_with_spawn_tool(
        session_id,
        root_id,
        backend.as_ref(),
        PermissionMode::Allow,
        AgentBudget {
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    );
    let (commands, sink, _supervisor, root_cancel, runner_task) =
        bootstrap(session_id, root_id, backend, root);

    let call_id = ToolCallId::new();
    send_spawn_tool_call(
        &commands,
        run_id,
        call_id,
        serde_json::json!({ "task": "summarize this", "mode": "await", "model": "claude-haiku-4-5" }),
    )
    .await;

    wait_for(
        || sink.tool_completed_output(root_id, call_id).is_some(),
        Duration::from_secs(5),
        "ToolCallCompleted for the spawn call",
    )
    .await;

    let recorded = recording_backend
        .requested_models
        .lock()
        .expect("requested_models lock poisoned")
        .clone();
    assert!(
        recorded
            .iter()
            .any(|model| model.as_deref() == Some("claude-haiku-4-5")),
        "the child's own backend call must carry the requested model override, got {recorded:?}"
    );

    root_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

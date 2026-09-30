use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionResult};
use harness_protocol::commands::UserInput;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{RequestId, SessionId, Timestamp};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_session_store::{JsonlSessionStore, SessionStore};

use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::testing::{FakeBackend, FakeToolRegistry};
use crate::traits::EventSink;
use crate::workspace::FakeWorkspace;

use super::*;

// -----------------------------------------------------------------------
// Helper: drain buffered events from a broadcast receiver non-blockingly
// -----------------------------------------------------------------------

fn drain_events(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

/// Drains buffered envelopes from a broadcast receiver non-blockingly.
fn drain_envelopes(rx: &mut broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEventEnvelope> {
    let mut envelopes = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        envelopes.push(envelope);
    }
    envelopes
}

// -----------------------------------------------------------------------
// Test: session event bus produces a well-ordered event stream
// -----------------------------------------------------------------------

/// Verifies that subscribing to the session event bus and sending
/// `SessionCommand::Prompt` produces a well-ordered stream.
///
/// The Phase 1 state machine emits these events for a
/// TextDelta → Completed sequence:
///   StateChanged(Idle → PreparingContext)
///   RunStarted
///   StateChanged(PreparingContext → Streaming)
///   AssistantTextDelta("Hello, world!")
///   StateChanged(Streaming → Idle)
///   Completed { Success }
#[tokio::test]
async fn session_prompt_produces_ordered_event_stream() {
    // ── Setup ────────────────────────────────────────
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: "Hello, world!".into(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: ExecutionResult {
                        request_id,
                        usage: ModelUsage::default(),
                        cost: Cost::default(),
                        finish_reason: "end_turn".into(),
                    },
                },
            ])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );

    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    // Use a no-op event sink for the external persistence/logging path.
    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
    );

    // Subscribe to the event bus before sending any command.
    let mut subscriber = runtime.event_bus.subscribe();

    // ── Act: send Prompt command ────────────────────────
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // ── Collect events ─────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..30 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
            if all_events
                .iter()
                .any(|e| matches!(e, AgentEvent::Completed { .. }))
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // ── Assert ──────────────────────────────────
    // Filter out StateChanged events to see the core sequence.
    let core_events: Vec<&AgentEvent> = all_events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::StateChanged { .. }))
        .collect();

    // Expected core events after StateChanged filtering:
    //   0. RunStarted
    //   1. AssistantTextDelta("Hello, world!")
    //   2. Completed { outcome: Success }
    assert!(
        core_events.len() >= 3,
        "expected at least 3 non-StateChanged events (RunStarted, AssistantTextDelta, Completed); got {}: {:?}",
        core_events.len(),
        core_events,
    );

    assert!(
        matches!(core_events[0], AgentEvent::RunStarted { .. }),
        "event[0] should be RunStarted, got {:?}",
        core_events[0]
    );

    assert!(
        matches!(&core_events[1], AgentEvent::AssistantTextDelta { delta, .. } if delta == "Hello, world!"),
        "event[1] should be AssistantTextDelta(\"Hello, world!\"), got {:?}",
        core_events[1]
    );

    assert!(
        matches!(core_events[2], AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Success),
        "event[2] should be Completed(Success), got {:?}",
        core_events[2]
    );

    // Verify ordering.
    let run_started_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::RunStarted { .. }));
    let delta_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::AssistantTextDelta { .. }));
    let completed_idx = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::Completed { .. }));

    assert!(
        run_started_idx < delta_idx,
        "RunStarted should precede AssistantTextDelta"
    );
    assert!(
        delta_idx < completed_idx,
        "AssistantTextDelta should precede Completed"
    );
}

// -----------------------------------------------------------------------
// Test: agent_live_state reflects live status transitions and usage
// -----------------------------------------------------------------------

/// Verifies that [`SessionRuntime::agent_live_state`] is a truthful,
/// continuously-updated projection: it starts `Idle`, transitions away
/// from `Idle` while the run is in flight, and ends with a recorded
/// `Success` outcome and non-empty usage once the run completes.
#[tokio::test]
async fn agent_live_state_reflects_run_lifecycle() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "hi".into(),
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage {
                    input_tokens: harness_protocol::usage::UsageValue::new(Some(5)),
                    output_tokens: harness_protocol::usage::UsageValue::new(Some(7)),
                    total_tokens: harness_protocol::usage::UsageValue::new(Some(12)),
                    ..Default::default()
                },
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
    );

    let root_id = runtime
        .state
        .lock()
        .expect("state mutex poisoned")
        .root_agent_id;

    // Before any command, live state is the default (Idle, no outcome).
    let before = runtime.agent_live_state(root_id);
    assert_eq!(before.status, AgentStatus::Idle);
    assert!(before.last_outcome.is_none());

    let mut subscriber = runtime.event_bus.subscribe();

    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // Poll until a Completed event has been observed.
    let mut completed = false;
    for _ in 0..50 {
        let batch = drain_events(&mut subscriber);
        if batch
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. }))
        {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "run should complete within the polling window");

    // Give the runner one more tick to publish the post-completion status.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let after = runtime.agent_live_state(root_id);
    assert_eq!(
        after.last_outcome,
        Some(AgentOutcome::Success),
        "last_outcome should be Success after the run completes"
    );
    assert_eq!(
        after.usage.inclusive_usage.total_tokens.value(),
        Some(12),
        "usage should be populated from the scripted ExecutionResult"
    );
    assert_eq!(after.total_requests, 1);
}

#[tokio::test]
async fn session_root_remains_available_across_completed_runs() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();
    let backend = Arc::new(FakeBackend::new().with_result(ExecutionResult {
        request_id,
        usage: ModelUsage::default(),
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    }));

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
    );
    let root_id = runtime.state_snapshot().root_agent_id;

    for prompt in ["first", "second"] {
        runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text: prompt.into(),
                attachments: vec![],
            }))
            .await
            .expect("root mailbox remains available");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let live = runtime.agent_live_state(root_id);
                let expected_requests = if prompt == "first" { 1 } else { 2 };
                if live.status == AgentStatus::Idle && live.total_requests >= expected_requests {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should complete");
        assert_eq!(runtime.state_snapshot().status, SessionStatus::Completed);
    }
}

#[tokio::test]
async fn partial_provider_stream_failure_has_a_truthful_terminal_state() {
    let session_id = SessionId::new();
    let request_id = RequestId::new();
    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "partial".into(),
            }])
            .with_error(ExecutionError::BackendError {
                message: "stream disconnected".into(),
                code: "SCRIPTED_DISCONNECT".into(),
            }),
    );

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new(
        session_id,
        backend,
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
    );
    let root_id = runtime.state_snapshot().root_agent_id;
    let mut subscriber = runtime.event_bus.subscribe();
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "stream".into(),
            attachments: vec![],
        }))
        .await
        .expect("start run");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime.agent_live_state(root_id).status == AgentStatus::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider failure should terminate the run");

    tokio::time::sleep(Duration::from_millis(20)).await;
    let events = drain_events(&mut subscriber);
    let partial_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::AssistantTextDelta { delta, .. } if delta == "partial"))
        .expect("partial delta is published");
    let failed_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::Failed { .. }))
        .expect("failure event is published");
    assert!(partial_index < failed_index);
    let snapshot = runtime.state_snapshot();
    assert_eq!(snapshot.status, SessionStatus::Failed);
    assert!(snapshot.error.is_some());
}

// -----------------------------------------------------------------------
// Test: RC-301 — stored and observed order agree through the committer
// -----------------------------------------------------------------------

/// With a durable store configured, events flow through the session's
/// authoritative committer: subscribers observe the exact sequences the
/// store persisted, ephemeral deltas are not persisted, and a terminal
/// run triggers a checkpoint (RC-302).
#[tokio::test]
async fn committed_events_match_stored_order() {
    let dir = std::env::temp_dir().join(format!(
        "harness-runtime-rc301-{}-{}",
        std::process::id(),
        Timestamp::now().timestamp_millis()
    ));
    let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(&dir));
    let session_id = SessionId::new();
    let request_id = RequestId::new();

    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: "hello".into(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: ExecutionResult {
                        request_id,
                        usage: ModelUsage::default(),
                        cost: Cost::default(),
                        finish_reason: "end_turn".into(),
                    },
                },
            ])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    let runtime = SessionRuntime::new_with_scheduler(
        session_id,
        backend,
        tool_registry,
        workspace,
        Arc::new(NoopSink),
        AgentToolset {
            tools: HashMap::new(),
        },
        Arc::new(Scheduler::new(SchedulerConfig::default())),
        Some(store.clone()),
    );

    let mut subscriber = runtime.event_bus.subscribe();
    runtime
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello".to_string(),
            attachments: vec![],
        }))
        .await
        .expect("send_command should succeed");

    // Collect observed sequences (committer-assigned) until the run
    // completes.
    let mut observed_sequences: Vec<u64> = Vec::new();
    let mut completed = false;
    for _ in 0..50 {
        let batch = drain_envelopes(&mut subscriber);
        for envelope in batch {
            if let Some(sequence) = envelope.session_sequence {
                observed_sequences.push(sequence);
            }
            if matches!(envelope.event, AgentEvent::Completed { .. }) {
                completed = true;
            }
        }
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "run should complete");

    // Wait for the terminal checkpoint to land in the store.
    let mut snapshot_seen = false;
    for _ in 0..50 {
        if let Ok(stored) = store.load_session(session_id).await {
            snapshot_seen = stored.snapshot.is_some();
            if snapshot_seen {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        snapshot_seen,
        "a terminal run must produce an automatic checkpoint"
    );

    // Every durable event the store persisted must carry a final sequence,
    // and the observed stream is strictly increasing overall (stored and
    // observed order agree on the durable events).
    let stored_events = store
        .events_since(session_id, 0)
        .await
        .expect("load committed event history");
    let stored_sequences: Vec<u64> = stored_events
        .iter()
        .filter_map(|event| event.session_sequence)
        .collect();
    assert!(
        !stored_sequences.is_empty(),
        "durable events were persisted with final sequences"
    );
    for pair in observed_sequences.windows(2) {
        assert!(
            pair[1] > pair[0],
            "observed sequences are strictly increasing"
        );
    }
    for stored_sequence in &stored_sequences {
        assert!(
            observed_sequences.contains(stored_sequence),
            "stored sequence {stored_sequence} was observed with the same value"
        );
    }

    // No streaming delta was persisted.
    assert!(
        stored_events
            .iter()
            .all(|event| !matches!(event.envelope.event, AgentEvent::AssistantTextDelta { .. })),
        "ephemeral deltas are never stored"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// -----------------------------------------------------------------------
// Behavior profiles: child agents
// -----------------------------------------------------------------------

mod child_behavior {
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use harness_core::behavior::{
        BehaviorState, ChildBehaviorResolver, ChildPolicyError, ProfileRef,
    };
    use harness_protocol::backend::{BackendCapabilities, BackendDescriptor, ExecutionRequest};
    use harness_protocol::effects::{
        BackendPolicy, SpawnAgentSpec, SpawnMode, ToolInheritance, WorkspacePolicy,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::traits::ExecutionBackend;

    /// Records every request, then completes it like `FakeBackend`.
    struct RecordingBackend {
        requests: Arc<StdMutex<Vec<ExecutionRequest>>>,
        inner: FakeBackend,
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

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    fn profile(id: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1, "id": id, "revision": 1, "name": id,
            "instructions": { "text": text }
        })
    }

    fn spec(task: &str) -> SpawnAgentSpec {
        SpawnAgentSpec {
            role: Some("Child role.".into()),
            backend: BackendPolicy::Inherit,
            tools: ToolInheritance::InheritAll,
            workspace: WorkspacePolicy::Inherit,
            budget: Default::default(),
            mode: SpawnMode::Concurrent,
            task: Some(task.into()),
            execution_params: Default::default(),
            origin_tool_call_id: None,
        }
    }

    /// Starts a session under `parent_profile`, spawns one child with a
    /// task, and returns the system prompts of the requests the child made.
    async fn child_prompts(resolver: Option<Arc<dyn ChildBehaviorResolver>>) -> Vec<String> {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let request_id = RequestId::new();
        let runtime = SessionRuntime::new(
            SessionId::new(),
            Arc::new(RecordingBackend {
                requests: requests.clone(),
                inner: FakeBackend::new().with_result(ExecutionResult {
                    request_id,
                    usage: ModelUsage::default(),
                    cost: Cost::default(),
                    finish_reason: "end_turn".into(),
                }),
            }),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
        );
        if let Some(resolver) = resolver {
            runtime.set_child_behavior_resolver(resolver);
        }
        runtime
            .send_command(SessionCommand::SetBehaviorProfile(profile(
                "parent",
                "PARENT PROFILE.",
            )))
            .await
            .unwrap();
        runtime
            .send_command(SessionCommand::SpawnChild(spec("child task")))
            .await
            .unwrap();

        // The child's request is the only one: the root never runs.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while requests.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let prompts = requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.system_prompt.clone())
            .collect();
        runtime.shutdown();
        prompts
    }

    #[tokio::test]
    async fn children_inherit_the_parent_profile_by_default() {
        let prompts = child_prompts(None).await;
        assert_eq!(prompts, vec!["Child role.\n\nPARENT PROFILE.".to_string()]);
    }

    #[derive(Debug)]
    struct Fixed(Arc<harness_core::behavior::CompiledProfile>);

    impl ChildBehaviorResolver for Fixed {
        fn resolve(
            &self,
            parent: &BehaviorState,
            _spec: &SpawnAgentSpec,
        ) -> Result<BehaviorState, ChildPolicyError> {
            assert_eq!(parent.reference(), ProfileRef::exact("parent", 1));
            Ok(BehaviorState::new(self.0.clone()))
        }
    }

    #[derive(Debug)]
    struct Refuse;

    impl ChildBehaviorResolver for Refuse {
        fn resolve(
            &self,
            _parent: &BehaviorState,
            _spec: &SpawnAgentSpec,
        ) -> Result<BehaviorState, ChildPolicyError> {
            Err(ChildPolicyError::Rejected("no children here".into()))
        }
    }

    #[tokio::test]
    async fn a_custom_resolver_decides_the_child_profile() {
        let child_profile = harness_core::behavior::compile(
            serde_json::from_value(profile("child-only", "CHILD PROFILE.")).unwrap(),
        )
        .unwrap();
        let prompts = child_prompts(Some(Arc::new(Fixed(Arc::new(child_profile))))).await;
        assert_eq!(prompts, vec!["Child role.\n\nCHILD PROFILE.".to_string()]);
    }

    #[tokio::test]
    async fn a_resolver_can_refuse_to_create_a_child() {
        assert!(child_prompts(Some(Arc::new(Refuse))).await.is_empty());
    }
}

// -----------------------------------------------------------------------
// Behavior profiles: completion gate evaluators, end to end
// -----------------------------------------------------------------------

mod completion_gate_e2e {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use harness_protocol::backend::{BackendCapabilities, BackendDescriptor, ExecutionRequest};
    use harness_protocol::ids::ToolId;
    use harness_protocol::tools::{PermissionMode, ToolCapability, ToolPolicy};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::traits::{ExecutionBackend, ToolExecutor};

    /// Plays both roles: the agent always answers "Done."; the verifier
    /// (recognised by its system prompt) answers from a script.
    struct TwoRoleBackend {
        inner: FakeBackend,
        verdicts: StdMutex<VecDeque<&'static str>>,
        agent_requests: StdMutex<Vec<ExecutionRequest>>,
        verifier_requests: StdMutex<Vec<ExecutionRequest>>,
    }

    impl TwoRoleBackend {
        fn new(verdicts: &[&'static str]) -> Arc<Self> {
            Arc::new(Self {
                inner: FakeBackend::new(),
                verdicts: StdMutex::new(verdicts.iter().copied().collect()),
                agent_requests: StdMutex::new(Vec::new()),
                verifier_requests: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ExecutionBackend for TwoRoleBackend {
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
            _cancel: CancellationToken,
        ) -> Result<ExecutionResult, ExecutionError> {
            let is_verifier = request
                .system_prompt
                .starts_with("You are a strict verifier");
            let reply = if is_verifier {
                self.verifier_requests.lock().unwrap().push(request.clone());
                self.verdicts
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("scripted verdict")
                    .to_string()
            } else {
                self.agent_requests.lock().unwrap().push(request.clone());
                "Done.".to_string()
            };
            let request_id = request.request_id;
            let _ = sink.send(ExecutionEvent::TextDelta {
                request_id,
                delta: reply,
            });
            Ok(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            })
        }
    }

    /// A test runner that fails its first `failures` runs.
    struct FlakyTests {
        failures: usize,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ToolExecutor for FlakyTests {
        fn descriptor(&self) -> crate::traits::ToolDescriptor {
            crate::traits::ToolDescriptor {
                id: harness_tools::ToolId::new("run_tests"),
                name: "run_tests".into(),
                description: "Run the test suite".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: crate::traits::ToolInput,
            _cancel: CancellationToken,
        ) -> Result<harness_tools::ToolResult, harness_tools::ToolError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let failing = call < self.failures;
            Ok(harness_tools::ToolResult {
                call_id: "test".into(),
                output: serde_json::json!(if failing {
                    "1 failed: test_null"
                } else {
                    "all passed"
                }),
                is_error: failing,
            })
        }
    }

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    /// Runs one prompt under a profile with `gate` and returns the gate
    /// events `(passed, continuing, failed_checks)` in order.
    async fn run_gated(
        backend: Arc<TwoRoleBackend>,
        tool: Option<(Arc<FlakyTests>, PermissionMode)>,
        gate: serde_json::Value,
    ) -> Vec<(bool, bool, Vec<String>)> {
        let mut registry = FakeToolRegistry::new();
        let mut toolset = AgentToolset {
            tools: HashMap::new(),
        };
        if let Some((executor, permission)) = tool {
            registry.add_executor(executor);
            let id = ToolId::new();
            toolset.tools.insert(
                id,
                ToolCapability {
                    descriptor: harness_protocol::tools::ToolDescriptor {
                        id,
                        name: "run_tests".into(),
                        description: "Run the test suite".into(),
                        input_schema: serde_json::json!({"type": "object"}),
                    },
                    policy: ToolPolicy {
                        permission,
                        enabled: true,
                    },
                    delegatable: false,
                },
            );
        }
        let runtime = SessionRuntime::new_with_toolset(
            SessionId::new(),
            backend,
            Arc::new(registry),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            toolset,
        );
        let mut events = runtime.event_bus.subscribe();
        runtime
            .send_command(SessionCommand::SetBehaviorProfile(serde_json::json!({
                "schema_version": 1, "id": "gated", "revision": 1, "name": "Gated",
                // The model never sees run_tests; only the gate uses it.
                "tools": { "type": "none" },
                "completion_gate": gate
            })))
            .await
            .unwrap();
        runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text: "fix the bug".into(),
                attachments: vec![],
            }))
            .await
            .unwrap();

        let mut gate_events = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await.expect("event stream").event {
                    AgentEvent::CompletionGateEvaluated {
                        passed,
                        continuing,
                        failed_checks,
                        ..
                    } => gate_events.push((passed, continuing, failed_checks)),
                    AgentEvent::Completed { .. } | AgentEvent::Failed { .. } => break,
                    _ => {}
                }
            }
        })
        .await
        .expect("run finishes");
        runtime.shutdown();
        gate_events
    }

    #[tokio::test]
    async fn a_tool_evaluator_keeps_the_agent_working_until_tests_pass() {
        let backend = TwoRoleBackend::new(&[]);
        let tests = Arc::new(FlakyTests {
            failures: 1,
            calls: AtomicUsize::new(0),
        });
        let events = run_gated(
            backend.clone(),
            Some((tests.clone(), PermissionMode::Allow)),
            serde_json::json!({
                "checks": [{ "id": "tests-green", "evaluator": { "type": "tool", "tool": "run_tests" } }]
            }),
        )
        .await;

        assert_eq!(
            events,
            vec![
                (false, true, vec!["tests-green".to_string()]),
                (true, false, vec![]),
            ]
        );
        assert_eq!(tests.calls.load(Ordering::SeqCst), 2);
        let agent_requests = backend.agent_requests.lock().unwrap();
        assert_eq!(agent_requests.len(), 2);
        assert!(
            agent_requests[1].tools.is_empty(),
            "the gate's tool stays hidden from the model"
        );
        let feedback = format!("{:?}", agent_requests[1].messages.last().unwrap());
        assert!(feedback.contains("1 failed: test_null"), "{feedback}");
    }

    #[tokio::test]
    async fn a_model_evaluator_judges_the_answer() {
        let backend = TwoRoleBackend::new(&[
            r#"{"passed": false, "feedback": "Explain the root cause."}"#,
            r#"{"passed": true, "feedback": ""}"#,
        ]);
        let events = run_gated(
            backend.clone(),
            None,
            serde_json::json!({
                "checks": [{ "id": "judge", "evaluator": {
                    "type": "model", "instructions": "The answer must explain the root cause."
                } }]
            }),
        )
        .await;

        assert_eq!(
            events,
            vec![
                (false, true, vec!["judge".to_string()]),
                (true, false, vec![])
            ]
        );
        let verifier = backend.verifier_requests.lock().unwrap();
        assert_eq!(verifier.len(), 2);
        assert!(verifier[0]
            .system_prompt
            .contains("The answer must explain the root cause."));
        assert!(verifier[0].params.response_format.is_some());
        let prompt = format!("{:?}", verifier[0].messages[0]);
        assert!(prompt.contains("<proposed_answer>\\nDone."), "{prompt}");
        let agent_requests = backend.agent_requests.lock().unwrap();
        assert!(format!("{:?}", agent_requests[1].messages.last().unwrap())
            .contains("Explain the root cause."));
    }

    #[tokio::test]
    async fn verifier_requests_count_as_the_agents_usage() {
        let backend = TwoRoleBackend::new(&[r#"{"passed": true, "feedback": ""}"#]);
        let runtime = SessionRuntime::new(
            SessionId::new(),
            backend.clone(),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
        );
        let root = runtime.state_snapshot().root_agent_id;
        runtime
            .send_command(SessionCommand::SetBehaviorProfile(serde_json::json!({
                "schema_version": 1, "id": "judged", "revision": 1, "name": "Judged",
                "completion_gate": { "checks": [{ "id": "judge",
                    "evaluator": { "type": "model", "instructions": "ok?" } }] }
            })))
            .await
            .unwrap();
        runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text: "go".into(),
                attachments: vec![],
            }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while runtime.agent_live_state(root).last_outcome.is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("run completes");
        assert_eq!(
            runtime.agent_live_state(root).total_requests,
            2,
            "one agent request and one verifier request"
        );
        runtime.shutdown();
    }

    #[tokio::test]
    async fn an_evaluator_that_cannot_run_does_not_pass_silently() {
        let backend = TwoRoleBackend::new(&[]);
        let tests = Arc::new(FlakyTests {
            failures: 0,
            calls: AtomicUsize::new(0),
        });
        let events = run_gated(
            backend,
            Some((tests.clone(), PermissionMode::Ask)),
            serde_json::json!({
                "checks": [{ "id": "tests-green", "evaluator": { "type": "tool", "tool": "run_tests" } }],
                "max_continuations": 0
            }),
        )
        .await;
        assert_eq!(
            events,
            vec![(false, false, vec!["tests-green".to_string()])]
        );
        assert_eq!(
            tests.calls.load(Ordering::SeqCst),
            0,
            "a tool that needs approval is never run by the gate"
        );
    }

    /// Like `run_gated`, but installs the profile as a bundle (with command
    /// trust) in a workspace rooted at `root`. Returns the gate events and
    /// the root agent's request count.
    async fn run_bundled(
        backend: Arc<TwoRoleBackend>,
        gate: serde_json::Value,
        root: &std::path::Path,
        allow_commands: bool,
    ) -> (Vec<(bool, bool, Vec<String>)>, u64) {
        let runtime = SessionRuntime::new(
            SessionId::new(),
            backend,
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new().with_root(root)),
            Arc::new(NoopSink),
        );
        let agent = runtime.state_snapshot().root_agent_id;
        let mut events = runtime.event_bus.subscribe();
        runtime
            .send_command(SessionCommand::SetBehaviorBundle {
                profile: serde_json::json!({
                    "schema_version": 1, "id": "gated", "revision": 1, "name": "Gated",
                    "completion_gate": gate
                }),
                library: Vec::new(),
                allow_commands,
            })
            .await
            .unwrap();
        runtime
            .send_command(SessionCommand::Prompt(UserInput {
                text: "fix the bug".into(),
                attachments: vec![],
            }))
            .await
            .unwrap();
        let mut gate_events = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let envelope = events.recv().await.expect("event stream");
                if envelope.agent_id != agent {
                    continue;
                }
                match envelope.event {
                    AgentEvent::CompletionGateEvaluated {
                        passed,
                        continuing,
                        failed_checks,
                        ..
                    } => gate_events.push((passed, continuing, failed_checks)),
                    AgentEvent::Completed { .. } | AgentEvent::Failed { .. } => break,
                    _ => {}
                }
            }
        })
        .await
        .expect("run finishes");
        // Usage from a verifier arrives with its verdict, before completion.
        let requests = runtime.agent_live_state(agent).total_requests;
        runtime.shutdown();
        (gate_events, requests)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_trusted_command_follows_the_stop_hook_contract() {
        let dir = tempfile::tempdir().unwrap();
        let gate = serde_json::json!({
            "checks": [{ "id": "hook", "evaluator": { "type": "command", "command":
                "cat > input.json; if [ -f attempted ]; then exit 0; fi; touch attempted; \
                 echo 'Run the linter first.' >&2; exit 2" } }]
        });
        let (events, _) = run_bundled(TwoRoleBackend::new(&[]), gate, dir.path(), true).await;
        assert_eq!(
            events,
            vec![
                (false, true, vec!["hook".to_string()]),
                (true, false, vec![])
            ]
        );
        let input: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("input.json")).unwrap())
                .unwrap();
        assert_eq!(input["hook_event_name"], "Stop");
        assert_eq!(input["last_assistant_message"], "Done.");
        assert_eq!(
            input["stop_hook_active"], true,
            "second check follows a rejection"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_untrusted_command_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let gate = serde_json::json!({
            "checks": [{ "id": "hook", "evaluator": { "type": "command", "command": "touch ran" } }],
            "max_continuations": 0
        });
        let (events, _) = run_bundled(TwoRoleBackend::new(&[]), gate, dir.path(), false).await;
        assert_eq!(events, vec![(false, false, vec!["hook".to_string()])]);
        assert!(!dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn an_agent_evaluator_runs_an_isolated_verifier() {
        let dir = tempfile::tempdir().unwrap();
        let backend = TwoRoleBackend::new(&[
            r#"{"passed": false, "feedback": "The root cause is not explained."}"#,
            r#"{"passed": true, "feedback": ""}"#,
        ]);
        let gate = serde_json::json!({
            "checks": [{ "id": "verifier", "evaluator": {
                "type": "agent", "instructions": "Check that the root cause is explained." } }]
        });
        let (events, requests) = run_bundled(backend.clone(), gate, dir.path(), false).await;
        assert_eq!(
            events,
            vec![
                (false, true, vec!["verifier".to_string()]),
                (true, false, vec![])
            ]
        );
        let verifier = backend.verifier_requests.lock().unwrap();
        assert_eq!(verifier.len(), 2);
        assert!(verifier[0]
            .system_prompt
            .contains("Check that the root cause is explained."));
        assert!(
            verifier[0].tools.is_empty(),
            "the verifier gets only the tools it names"
        );
        assert_eq!(requests, 4, "two agent requests and two verifier requests");
        let agent_requests = backend.agent_requests.lock().unwrap();
        assert!(format!("{:?}", agent_requests[1].messages.last().unwrap())
            .contains("The root cause is not explained."));
    }
}

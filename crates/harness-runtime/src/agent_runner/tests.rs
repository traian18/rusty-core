use std::collections::HashMap as StdHashMap;
use std::sync::Mutex;
use std::time::Duration;

use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionResult,
};
use harness_protocol::commands::UserInput;
use harness_protocol::ids::{BackendId, ConfigurationId, IntegrationId, RequestId, SessionId};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;

use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::testing::{FakeBackend, FakeToolExecutor, FakeToolRegistry};
use crate::workspace::FakeWorkspace;

use super::*;

#[tokio::test]
async fn send_command_through_mailbox() {
    let (mut task, sender) = AgentTask::new(AgentId::new());
    sender
        .send(AgentCommand::Cancel)
        .await
        .expect("send command");
    assert!(matches!(
        task.commands.recv().await,
        Some(AgentCommand::Cancel)
    ));
}

struct NoopSink;
impl EventSink for NoopSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}
/// The mailbox is bounded (64) and the backend sink is a lossy
/// broadcast: a forwarder that awaits `commands.send` while the mailbox
/// is full stops receiving, and every event the backend streams past the
/// sink's capacity in the meantime is silently overwritten. Here nobody
/// reads the mailbox until the whole burst has been sent, so the old
/// forwarder lost all but the last `BACKEND_EVENT_BUFFER` deltas.
#[tokio::test]
async fn forwarder_keeps_draining_the_sink_while_the_mailbox_is_full() {
    let run_id = RunId::new();
    let request_id = RequestId::new();
    let total = BACKEND_EVENT_BUFFER * 4;
    let (event_tx, event_rx) = broadcast::channel(BACKEND_EVENT_BUFFER);
    let (commands_tx, mut commands_rx) = mpsc::channel(4);
    let forwarder = tokio::spawn(forward_backend_events(
        event_rx,
        commands_tx,
        run_id,
        CancellationToken::new(),
    ));

    for index in 0..total {
        let _ = event_tx.send(ExecutionEvent::TextDelta {
            request_id,
            delta: index.to_string(),
        });
        // Give the forwarder a turn the way a real backend's I/O awaits
        // do, without ever draining the mailbox.
        if index % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }
    let _ = event_tx.send(ExecutionEvent::Completed {
        request_id,
        result: ExecutionResult {
            request_id,
            usage: Default::default(),
            cost: Default::default(),
            finish_reason: "stop".into(),
        },
    });
    drop(event_tx);

    let mut received = Vec::new();
    while let Some(AgentCommand::BackendEvent { event, .. }) = commands_rx.recv().await {
        match event {
            ExecutionEvent::TextDelta { delta, .. } => received.push(delta),
            ExecutionEvent::Completed { .. } => break,
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(
        forwarder.await.expect("forwarder join"),
        "terminal event was forwarded"
    );
    assert_eq!(received.len(), total, "every delta must reach the mailbox");
    assert!(
        received
            .iter()
            .enumerate()
            .all(|(index, delta)| delta == &index.to_string()),
        "deltas must arrive in order"
    );
}

fn test_agent(agent_id: AgentId, session_id: SessionId) -> Agent {
    Agent::new(
        agent_id,
        session_id,
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
            tools: AgentToolset {
                tools: StdHashMap::new(),
            },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: false,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

/// Builds a test agent with a single tool whose policy requires
/// permission (`PermissionMode::Ask`), used to exercise the
/// `WaitingForPermission` state.
fn test_agent_with_ask_permission_tool(
    agent_id: AgentId,
    session_id: SessionId,
    tool_name: &str,
) -> Agent {
    let tool_id = harness_protocol::ids::ToolId::new();
    let mut tools = StdHashMap::new();
    tools.insert(
        tool_id,
        harness_protocol::tools::ToolCapability {
            descriptor: harness_protocol::tools::ToolDescriptor {
                id: tool_id,
                name: tool_name.into(),
                description: "ask-permission test tool".into(),
                input_schema: serde_json::json!({}),
            },
            policy: harness_protocol::tools::ToolPolicy {
                permission: harness_protocol::tools::PermissionMode::Ask,
                enabled: true,
            },
            delegatable: false,
        },
    );
    Agent::new(
        agent_id,
        session_id,
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
                can_read: false,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

/// Task 2.3 acceptance criterion: start a `FakeBackend` call that blocks
/// until cancelled, cancel the token mid-flight, and assert the runner
/// transitions the agent to `Cancelled` and stops emitting further
/// backend events.
#[tokio::test]
async fn cancel_mid_flight_backend_call_transitions_to_cancelled() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);

    let (task, sender) = AgentTask::new(agent_id);
    let backend = Arc::new(FakeBackend::new().blocking_until_cancelled());
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    );

    let mut events_rx = runner.task.events.subscribe();

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "hi".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send StartRun");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    // Give the runner time to process StartRun and spawn the (blocking)
    // backend call before we cancel mid-flight.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Mid-flight cancellation.
    cancel.cancel();

    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should stop promptly after cancellation")
        .expect("runner task should not panic");

    assert_eq!(
        runner.agent.state.status,
        AgentStatus::Cancelled,
        "agent should transition to Cancelled after mid-flight cancellation"
    );

    // The live-state table should also reflect Cancelled.
    let live = live_state
        .lock()
        .expect("live_state mutex poisoned")
        .get(&agent_id)
        .cloned()
        .expect("live state entry exists");
    assert_eq!(live.status, AgentStatus::Cancelled);

    while events_rx.try_recv().is_ok() {}
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

/// RST-002: a runner opted into `.long_lived(true)` must keep processing
/// its mailbox after a run finishes, so the same agent/session can
/// accept a second `StartRun` without being recreated.
#[tokio::test]
async fn long_lived_runner_accepts_a_second_start_run_after_completion() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);

    let (task, sender) = AgentTask::new(agent_id);
    let request_id = harness_protocol::ids::RequestId::new();
    let backend = Arc::new(FakeBackend::new().with_result(
        harness_protocol::backend::ExecutionResult {
            request_id,
            usage: harness_protocol::usage::ModelUsage::default(),
            cost: harness_protocol::usage::Cost::default(),
            finish_reason: "end_turn".into(),
        },
    ));
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    )
    .long_lived(true);

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "first".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send first StartRun");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    // Wait for the first run to complete (status returns to Idle with a
    // recorded Success outcome) while the mailbox loop is still alive.
    let mut first_completed = false;
    for _ in 0..100 {
        let live = live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .cloned();
        if let Some(live) = live {
            if live.status == AgentStatus::Idle
                && matches!(live.last_outcome, Some(AgentOutcome::Success))
            {
                first_completed = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(first_completed, "first run should complete");

    // A second StartRun on the same mailbox must still be accepted and
    // processed — proving the runner task did not exit after the first
    // run's FinishRun effect.
    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "second".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send second StartRun on the still-alive mailbox");

    let mut second_completed = false;
    for _ in 0..100 {
        let live = live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .cloned();
        if let Some(live) = live {
            if live.status == AgentStatus::Idle && live.total_requests >= 2 {
                second_completed = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        second_completed,
        "a long-lived runner must process a second StartRun on the same mailbox"
    );

    drop(sender);
    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should exit once the mailbox is closed")
        .expect("runner task should not panic");
    assert_eq!(runner.agent.state.status, AgentStatus::Idle);
}

#[tokio::test]
async fn cancel_is_processed_while_backend_waits_for_scheduler_capacity() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);
    let (task, sender) = AgentTask::new(agent_id);
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
        max_concurrent_backend_requests: 0,
        ..SchedulerConfig::default()
    }));
    let mut runner = AgentRunner::new(
        agent,
        task,
        Arc::new(FakeBackend::new().blocking_until_cancelled()),
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel,
        live_state.clone(),
        scheduler,
    );

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "wait for capacity".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("start run");
    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .is_some_and(|state| state.status == AgentStatus::PreparingContext)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run should reach its scheduler-capacity wait");
    sender.send(AgentCommand::Cancel).await.expect("cancel run");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .is_some_and(|state| state.status == AgentStatus::Cancelled)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("capacity wait must not block cancellation");

    drop(sender);
    let runner = tokio::time::timeout(Duration::from_secs(1), run_handle)
        .await
        .expect("cancelled capacity wait should release mailbox senders")
        .expect("runner task should not panic");
    assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
}

#[tokio::test]
async fn strict_commit_failure_terminates_with_truthful_failed_state() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);
    let (task, sender) = AgentTask::new(agent_id);
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let store = Arc::new(harness_session_store::testing::MemoryStore::new());
    store.set_fail_appends(true);
    let committer = Arc::new(SessionCommitter::new(store, session_id));
    let mut runner = AgentRunner::new(
        agent,
        task,
        Arc::new(FakeBackend::new().blocking_until_cancelled()),
        Arc::new(FakeToolRegistry::new()),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        CancellationToken::new(),
        live_state.clone(),
        Arc::new(Scheduler::new(SchedulerConfig::default())),
    )
    .with_committer(committer)
    .long_lived(true);

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "must be durable".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("start run");
    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    let mut runner = tokio::time::timeout(Duration::from_secs(1), run_handle)
        .await
        .expect("strict persistence failure must terminate the runner")
        .expect("runner task should not panic");
    assert_eq!(runner.agent.state.status, AgentStatus::Failed);
    assert!(runner.agent.state.active_run.is_none());
    let error = runner
        .take_final_result()
        .expect("fatal result")
        .expect_err("strict persistence failure must be an error");
    assert_eq!(error.code, "PERSISTENCE_COMMIT_FAILED");
    assert_eq!(
        live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .expect("live state")
            .status,
        AgentStatus::Failed
    );
}

/// M2: a `FollowUp` sent while a run is active must queue FIFO
/// (`Agent::queued_inputs`) rather than starting immediately, and the
/// long-lived runner's automatic `StartNextQueuedRun` drain must start
/// it only after the first run genuinely finishes.
#[tokio::test]
async fn follow_up_queued_while_run_active_starts_after_first_completes() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);
    let (task, sender) = AgentTask::new(agent_id);
    let request_id = harness_protocol::ids::RequestId::new();
    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::TextDelta {
                request_id,
                delta: "partial".into(),
            }])
            .with_result(harness_protocol::backend::ExecutionResult {
                request_id,
                usage: harness_protocol::usage::ModelUsage::default(),
                cost: harness_protocol::usage::Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    )
    .long_lived(true);

    let mut events_rx = runner.task.events.subscribe();

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "first".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send first StartRun");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    // Wait until the first run is genuinely in flight (Streaming) before
    // sending the follow-up, so it is guaranteed to be queued rather
    // than started immediately.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .is_some_and(|state| state.status == AgentStatus::Streaming)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first run should reach Streaming");

    sender
        .send(AgentCommand::FollowUp {
            input: UserInput {
                text: "second".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send follow-up while first run is active");

    // Collect events in order until we observe two RunStarted events,
    // proving the follow-up was queued (not started immediately) and
    // only began after the first run's Completed/FinishRun.
    let mut run_started_count = 0;
    let mut saw_first_completed_before_second_start = false;
    let mut first_completed_seen = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(envelope) = events_rx.recv().await {
            match envelope.event {
                AgentEvent::Completed { .. } => {
                    first_completed_seen = true;
                }
                AgentEvent::RunStarted { .. } => {
                    run_started_count += 1;
                    if run_started_count == 2 {
                        saw_first_completed_before_second_start = first_completed_seen;
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("should observe a second RunStarted for the queued follow-up");

    assert_eq!(
        run_started_count, 2,
        "the follow-up must eventually start its own run"
    );
    assert!(
        saw_first_completed_before_second_start,
        "the queued follow-up must not start until the first run completed"
    );

    drop(sender);
    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should exit once the mailbox is closed")
        .expect("runner task should not panic");
    assert!(
        runner.agent.state.queued_inputs.is_empty(),
        "the queue must be drained once the follow-up run has started"
    );
}

/// M2: the runner's long-lived loop drains the next queued input
/// (`StartNextQueuedRun`) after *any* `FinishRun` effect, not just a
/// successful completion — `fail()` emits `FinishRun` exactly like a
/// success or a cancellation does. This locks that behavior down for the
/// failure case specifically: a follow-up queued while a run is active
/// must still start its own run after the first run fails, not be
/// stranded in `queued_inputs` forever.
#[tokio::test]
async fn follow_up_queued_while_run_active_starts_after_first_run_fails() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);
    let (task, sender) = AgentTask::new(agent_id);
    let backend = Arc::new(FakeBackend::new().with_error(
        harness_protocol::backend::ExecutionError::BackendError {
            message: "simulated transient failure".into(),
            code: "TEMPORARY".into(),
        },
    ));
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    )
    .long_lived(true);

    let mut events_rx = runner.task.events.subscribe();

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "first".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send first StartRun");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .is_some_and(|state| state.status == AgentStatus::PreparingContext)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first run should reach PreparingContext");

    sender
        .send(AgentCommand::FollowUp {
            input: UserInput {
                text: "second".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("queue a follow-up while the first run is active");

    let mut run_started_count = 0;
    let mut saw_first_failed_before_second_start = false;
    let mut first_failed_seen = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(envelope) = events_rx.recv().await {
            match envelope.event {
                AgentEvent::Failed { .. } => {
                    first_failed_seen = true;
                }
                AgentEvent::RunStarted { .. } => {
                    run_started_count += 1;
                    if run_started_count == 2 {
                        saw_first_failed_before_second_start = first_failed_seen;
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("should observe a second RunStarted for the queued follow-up after the failure");

    assert_eq!(
        run_started_count, 2,
        "the follow-up must eventually start its own run after the first run fails"
    );
    assert!(
        saw_first_failed_before_second_start,
        "the queued follow-up must not start until the first run has failed"
    );

    drop(sender);
    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should exit once the mailbox is closed")
        .expect("runner task should not panic");
    assert!(
        runner.agent.state.queued_inputs.is_empty(),
        "the queue must be drained once the follow-up run has started"
    );
}

#[tokio::test]
async fn dispatch_rejects_a_forged_allow_for_an_unapproved_or_denied_tool() {
    for mode in [
        harness_protocol::tools::PermissionMode::Ask,
        harness_protocol::tools::PermissionMode::Deny,
    ] {
        let agent_id = AgentId::new();
        let mut agent =
            test_agent_with_ask_permission_tool(agent_id, SessionId::new(), "write_file");
        for capability in agent.capabilities.tools.tools.values_mut() {
            capability.policy.permission = mode.clone();
        }
        let (task, _sender) = AgentTask::new(agent_id);
        let mut registry = FakeToolRegistry::new();
        registry.add_executor(Arc::new(FakeToolExecutor::new(
            harness_tools::ToolDescriptor {
                id: harness_tools::ToolId::new("write_file"),
                name: "write_file".into(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
        )));
        let mut runner = AgentRunner::new(
            agent,
            task,
            Arc::new(FakeBackend::new()),
            Arc::new(registry),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            CancellationToken::new(),
            Arc::new(Mutex::new(StdHashMap::new())),
            Arc::new(Scheduler::new(SchedulerConfig::default())),
        );
        let call_id = ToolCallId::new();
        runner
            .execute_tool(ToolRequest {
                call: harness_protocol::tools::ToolCall {
                    id: call_id,
                    name: "write_file".into(),
                    arguments: serde_json::json!({}),
                },
                permission: harness_protocol::tools::PermissionMode::Allow,
            })
            .await;
        assert!(matches!(
            runner.task.commands.recv().await,
            Some(AgentCommand::ToolFailed {
                error: harness_protocol::tools::ToolError::PermissionDenied,
                ..
            })
        ));
    }
}

/// M3: `shell.exec` calls must be bounded separately from generic tool
/// concurrency via `SchedulerConfig::max_concurrent_processes` — a real
/// OS process is heavier than "a tool call" in general. This proves the
/// wiring in `execute_tool` specifically, not just that the underlying
/// semaphore serializes (that's a given): with tool-execution
/// concurrency generous (10) but process concurrency exhausted (1, held
/// by an in-flight `shell.exec` call), a second `shell.exec` call must
/// block *before* ever reaching the tool executor. We prove that by
/// cancelling the second call's own token while it's still blocked: if
/// it were gated correctly, cancellation resolves the scheduler wait
/// with `None` and the call returns without ever invoking the executor
/// — so no `ToolCompleted`/`ToolFailed` command is ever sent for it. If
/// it were *not* gated (the bug this test guards against), the second
/// call would have already reached the executor and blocked on
/// `cancel.cancelled()` *inside* `FakeToolExecutor`, which resolves
/// cancellation by sending `ToolFailed` — a directly observable
/// difference.
#[tokio::test]
async fn shell_exec_calls_are_bounded_by_max_concurrent_processes() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let mut agent = test_agent_with_ask_permission_tool(agent_id, session_id, "shell.exec");
    for capability in agent.capabilities.tools.tools.values_mut() {
        capability.policy.permission = harness_protocol::tools::PermissionMode::Allow;
    }
    let (task, _sender) = AgentTask::new(agent_id);

    let mut registry = FakeToolRegistry::new();
    registry.add_executor(Arc::new(
        FakeToolExecutor::new(harness_tools::ToolDescriptor {
            id: harness_tools::ToolId::new("shell.exec"),
            name: "shell.exec".into(),
            description: "fake shell.exec".into(),
            input_schema: serde_json::json!({}),
        })
        .blocking_until_cancelled(),
    ));

    let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
        max_concurrent_tool_executions: 10,
        max_concurrent_processes: 1,
        ..SchedulerConfig::default()
    }));
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        Arc::new(FakeBackend::new()),
        Arc::new(registry),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel,
        live_state,
        scheduler,
    );

    let first_call = harness_protocol::ids::ToolCallId::new();
    runner
        .execute_tool(ToolRequest {
            call: harness_protocol::tools::ToolCall {
                id: first_call,
                name: "shell.exec".into(),
                arguments: serde_json::json!({}),
            },
            permission: harness_protocol::tools::PermissionMode::Allow,
        })
        .await;

    // Give the first call's spawned task time to actually acquire both
    // permits and enter the executor's blocking wait.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second_call = harness_protocol::ids::ToolCallId::new();
    runner
        .execute_tool(ToolRequest {
            call: harness_protocol::tools::ToolCall {
                id: second_call,
                name: "shell.exec".into(),
                arguments: serde_json::json!({}),
            },
            permission: harness_protocol::tools::PermissionMode::Allow,
        })
        .await;

    // Give the second call's spawned task time to reach (and block on)
    // whatever it's actually going to block on.
    tokio::time::sleep(Duration::from_millis(50)).await;
    runner.cancel_tool(second_call);

    // Drain the mailbox briefly: if the second call had reached the
    // executor (not gated), cancelling it produces a ToolFailed for
    // `second_call` promptly.
    let saw_second_call_complete =
        tokio::time::timeout(Duration::from_millis(200), runner.task.commands.recv()).await;
    match saw_second_call_complete {
        Ok(Some(AgentCommand::ToolFailed { call_id, .. }))
        | Ok(Some(AgentCommand::ToolCompleted { call_id, .. }))
            if call_id == second_call =>
        {
            panic!(
                "the second shell.exec call reached the executor instead of blocking \
                     on the exhausted process permit — max_concurrent_processes is not enforced"
            );
        }
        _ => {}
    }
}

/// A tool that never returns must not stall the run: once the scheduler's
/// `tool_call_timeout` elapses the call is cancelled and the core sees `ToolFailed(Timeout)`.
#[tokio::test]
async fn hung_tool_call_times_out_with_error_result() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let mut agent = test_agent_with_ask_permission_tool(agent_id, session_id, "fs.hang");
    for capability in agent.capabilities.tools.tools.values_mut() {
        capability.policy.permission = harness_protocol::tools::PermissionMode::Allow;
    }
    let (task, _sender) = AgentTask::new(agent_id);

    let mut registry = FakeToolRegistry::new();
    registry.add_executor(Arc::new(
        FakeToolExecutor::new(harness_tools::ToolDescriptor {
            id: harness_tools::ToolId::new("fs.hang"),
            name: "fs.hang".into(),
            description: "fake hanging tool".into(),
            input_schema: serde_json::json!({}),
        })
        .blocking_until_cancelled(),
    ));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
        tool_call_timeout: Duration::from_millis(100),
        ..SchedulerConfig::default()
    }));
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let mut runner = AgentRunner::new(
        agent,
        task,
        Arc::new(FakeBackend::new()),
        Arc::new(registry),
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        CancellationToken::new(),
        live_state,
        scheduler,
    );

    let call_id = harness_protocol::ids::ToolCallId::new();
    runner
        .execute_tool(ToolRequest {
            call: harness_protocol::tools::ToolCall {
                id: call_id,
                name: "fs.hang".into(),
                arguments: serde_json::json!({}),
            },
            permission: harness_protocol::tools::PermissionMode::Allow,
        })
        .await;

    let command = tokio::time::timeout(Duration::from_secs(2), runner.task.commands.recv())
        .await
        .expect("a hung tool must resolve once the tool timeout elapses");
    match command {
        Some(AgentCommand::ToolFailed { call_id: id, error }) => {
            assert_eq!(id, call_id);
            assert!(matches!(error, harness_protocol::tools::ToolError::Timeout));
        }
        other => panic!("expected ToolFailed(Timeout), got {other:?}"),
    }
}

/// M2: `AgentCommand::Cancel` is documented as cancelling only the
/// *current run* (see `AgentCommand::Cancel`'s doc comment and RC-203's
/// `multiple_follow_ups_are_fifo_and_survive_cancellation`). A follow-up
/// queued while a run was active is already-committed user intent and
/// must survive session/runner-level cancellation too, not just an
/// explicit `AgentCommand::Cancel` applied directly to the core state
/// machine — otherwise a queued follow-up typed just before a crash or a
/// host-initiated teardown would be silently lost, which the `Agent`
/// value itself must not misrepresent to a caller or restore path that
/// inspects it afterward.
#[tokio::test]
async fn queued_follow_up_survives_runner_cancellation() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let agent = test_agent(agent_id, session_id);
    let (task, sender) = AgentTask::new(agent_id);
    let backend = Arc::new(FakeBackend::new().blocking_until_cancelled());
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    )
    .long_lived(true);

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "first".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send first StartRun");
    sender
        .send(AgentCommand::FollowUp {
            input: UserInput {
                text: "queued".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("queue a follow-up while the first run is active");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should stop promptly after cancellation")
        .expect("runner task should not panic");

    assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
    assert_eq!(
        runner.agent.state.queued_inputs.len(),
        1,
        "cancellation must not discard already-queued follow-up/steer input"
    );
    assert_eq!(
        runner.agent.state.queued_inputs[0].text, "queued",
        "the surviving queued input must be the one sent before cancellation"
    );
}

/// M2: cancellation must be processed while the agent is
/// `WaitingForPermission`, and must not leave stale `pending_permissions`
/// or `pending_tools` state behind.
#[tokio::test]
async fn cancel_while_waiting_for_permission_transitions_to_cancelled() {
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let tool_name = "ask.tool";
    let agent = test_agent_with_ask_permission_tool(agent_id, session_id, tool_name);
    let (task, sender) = AgentTask::new(agent_id);
    let call_id = harness_protocol::ids::ToolCallId::new();
    let request_id = harness_protocol::ids::RequestId::new();
    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::ToolCallRequested {
                request_id,
                call: harness_protocol::tools::ToolCall {
                    id: call_id,
                    name: tool_name.into(),
                    arguments: serde_json::json!({}),
                },
            }])
            .blocking_until_cancelled(),
    );
    let tool_registry = Arc::new(FakeToolRegistry::new());
    let cancel = CancellationToken::new();
    let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
    let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

    let mut runner = AgentRunner::new(
        agent,
        task,
        backend,
        tool_registry,
        Arc::new(FakeWorkspace::new()),
        Arc::new(NoopSink),
        cancel.clone(),
        live_state.clone(),
        scheduler,
    );

    sender
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "please".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("start run");

    let run_handle = tokio::spawn(async move {
        runner.run().await;
        runner
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .is_some_and(|state| state.status == AgentStatus::WaitingForPermission)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run should reach WaitingForPermission");

    cancel.cancel();

    let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
        .await
        .expect("runner should stop promptly after cancellation while waiting for permission")
        .expect("runner task should not panic");

    assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
    assert!(runner.agent.state.pending_permissions.is_empty());
    assert!(runner.agent.state.pending_tools.is_empty());
    assert_eq!(
        live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .expect("live state")
            .status,
        AgentStatus::Cancelled
    );
}

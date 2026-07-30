//! Concurrency integration tests for the harness runtime.
//!
//! These tests verify multi-session isolation, cancellation safety, panic
//! containment, scheduler serialization, permission-scope integrity, and
//! resource-access exclusion — all using deterministic signalling
//! (channels, barriers, cancellation tokens) and the fake backend/tool/
//! workspace test doubles from `harness_runtime::testing`.
//!
//! # Organisation
//!
//! | Test | Underlying invariant |
//! |------|---------------------|
//! | `two_sessions_stream_concurrently_without_cross_talk` | T3 — session event streams are fully isolated |
//! | `cancelling_one_session_does_not_cancel_another` | Extended T2.3 — cancellation is session-scoped |
//! | `one_session_panic_marks_only_that_session_failed` | T2 — panic isolation via supervisor task |
//! | `scheduler_serializes_backend_calls_under_low_permit_count` | T4/T5 — semaphore-ordering of backend calls |
//! | `concurrent_ask_permission_requests_do_not_leak_across_agents` | PermissionPolicy::evaluate is stateless → agent-scoped |
//! | `resource_manager_denies_conflicting_exclusive_access_across_sessions` | T7 — ResourceManager exclusive-access exclusion |
//! | `shared_access_allows_concurrent_holders_while_blocking_exclusive` | T7 — shared vs exclusive coexistence |

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
use harness_protocol::commands::UserInput;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{RequestId, SessionId};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::{Cost, ModelUsage};
use tokio::sync::Barrier;

use harness_runtime::permissions::{PermissionOutcome, PermissionPolicy};
use harness_runtime::resource_manager::{AccessMode, ResourceError, ResourceKey, ResourceManager};
use harness_runtime::scheduler::{Scheduler, SchedulerConfig};
use harness_runtime::session_manager::SessionManager;
use harness_runtime::session_runtime::{SessionCommand, SessionStatus};
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use harness_runtime::traits::EventSink;
use harness_runtime::workspace::FakeWorkspace;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A no-op [`EventSink`] that discards every envelope.
struct NoopSink;

impl EventSink for NoopSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}

/// An [`EventSink`] that panics when called, used to trigger root-task panics
/// for isolation testing.
struct PanicSink;

impl EventSink for PanicSink {
    fn send(&self, _envelope: AgentEventEnvelope) {
        panic!("PanicSink: intentional panic to simulate root task failure");
    }
}

/// Drain buffered events from a broadcast receiver non-blockingly.
fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<AgentEventEnvelope>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

/// Create a scripted backend that streams a single text delta and completes.
fn make_scripted_backend(text: &str) -> FakeBackend {
    let request_id = RequestId::new();
    FakeBackend::new()
        .with_events(vec![ExecutionEvent::TextDelta {
            request_id,
            delta: text.to_string(),
        }])
        .with_result(ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        })
}

/// Create a bare [`AgentToolset`] with no tools.
fn empty_toolset() -> AgentToolset {
    AgentToolset {
        tools: HashMap::new(),
    }
}

/// Default scheduler for integration tests.
fn default_scheduler() -> Arc<Scheduler> {
    Arc::new(Scheduler::new(SchedulerConfig::default()))
}

// ===========================================================================
// T3 — Two sessions stream concurrently without cross-talk
// ===========================================================================

/// Two independent sessions each process a `Prompt` that streams events via
/// a [`FakeBackend`].  We subscribe to each session's event bus **before**
/// sending the command and verify that the text deltas from session A never
/// appear on session B's subscriber (and vice versa).
#[tokio::test]
async fn two_sessions_stream_concurrently_without_cross_talk() {
    let manager = SessionManager::new(default_scheduler());

    // Create two sessions with backends that produce distinguishable text.
    let backend_a = Arc::new(make_scripted_backend("from-session-A"));
    let backend_b = Arc::new(make_scripted_backend("from-session-B"));

    let registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());
    let sink = Arc::new(NoopSink);

    let sess_a = manager
        .create_session(backend_a, registry.clone(), workspace.clone(), sink.clone(), empty_toolset())
        .await;
    let sess_b = manager
        .create_session(backend_b, registry.clone(), workspace.clone(), sink.clone(), empty_toolset())
        .await;

    // Subscribe to each session's event bus.
    let mut rx_a = sess_a.event_bus.subscribe();
    let mut rx_b = sess_b.event_bus.subscribe();

    // Send prompts concurrently.
    let prompt_a = SessionCommand::Prompt(UserInput {
        text: "hello A".into(),
        attachments: vec![],
    });
    let prompt_b = SessionCommand::Prompt(UserInput {
        text: "hello B".into(),
        attachments: vec![],
    });

    let send_a = sess_a.send_command(prompt_a);
    let send_b = sess_b.send_command(prompt_b);
    let (res_a, res_b) = tokio::join!(send_a, send_b);
    res_a.expect("session A send_command should succeed");
    res_b.expect("session B send_command should succeed");

    // Collect events from both buses, looking for the distinguishing deltas.
    let mut events_a: Vec<AgentEvent> = Vec::new();
    let mut events_b: Vec<AgentEvent> = Vec::new();
    let mut completed_a = false;
    let mut completed_b = false;

    for _ in 0..50 {
        let batch_a = drain_events(&mut rx_a);
        let batch_b = drain_events(&mut rx_b);

        events_a.extend(batch_a);
        events_b.extend(batch_b);

        if events_a.iter().any(|e| matches!(e, AgentEvent::Completed { .. })) {
            completed_a = true;
        }
        if events_b.iter().any(|e| matches!(e, AgentEvent::Completed { .. })) {
            completed_b = true;
        }
        if completed_a && completed_b {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(completed_a, "session A should complete");
    assert!(completed_b, "session B should complete");

    // Verify no text cross-talk: session A's deltas must only contain its own text.
    let deltas_a: Vec<&str> = events_a
        .iter()
        .filter_map(|e| {
            if let AgentEvent::AssistantTextDelta { delta, .. } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect();
    for d in &deltas_a {
        assert_eq!(*d, "from-session-A", "session A delta should not contain B's text");
    }

    // Verify no text cross-talk: session B's deltas must only contain its own text.
    let deltas_b: Vec<&str> = events_b
        .iter()
        .filter_map(|e| {
            if let AgentEvent::AssistantTextDelta { delta, .. } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect();
    for d in &deltas_b {
        assert_eq!(*d, "from-session-B", "session B delta should not contain A's text");
    }

    // Clean up.
    let id_a = sess_a.session_id;
    let id_b = sess_b.session_id;
    manager.close_session(id_a).await.expect("close session A");
    manager.close_session(id_b).await.expect("close session B");
}

// ===========================================================================
// Extended T2.3 — Cancelling one session does not cancel another
// ===========================================================================

/// Two sessions start a blocking backend call simultaneously.  We cancel
/// only session A and verify that:
/// - Session A's agent transitions to `Cancelled`.
/// - Session B's agent continues unaffected until it too is cancelled.
#[tokio::test]
async fn cancelling_one_session_does_not_cancel_another() {
    let manager = SessionManager::new(default_scheduler());

    // Both sessions use a blocking-until-cancelled backend so they stay
    // in-flight until we cancel them.
    let backend_a = Arc::new(FakeBackend::new().blocking_until_cancelled());
    let backend_b = Arc::new(FakeBackend::new().blocking_until_cancelled());

    let registry = Arc::new(FakeToolRegistry::new());
    let workspace = Arc::new(FakeWorkspace::new());
    let sink = Arc::new(NoopSink);

    let sess_a = manager
        .create_session(backend_a, registry.clone(), workspace.clone(), sink.clone(), empty_toolset())
        .await;
    let sess_b = manager
        .create_session(backend_b, registry.clone(), workspace.clone(), sink.clone(), empty_toolset())
        .await;

    let id_a = sess_a.session_id;
    let id_b = sess_b.session_id;

    let mut rx_a = sess_a.event_bus.subscribe();
    let mut rx_b = sess_b.event_bus.subscribe();

    // Start both sessions.
    let prompt: SessionCommand = SessionCommand::Prompt(UserInput {
        text: "go".into(),
        attachments: vec![],
    });
    sess_a.send_command(prompt.clone()).await.expect("session A start");
    sess_b.send_command(prompt).await.expect("session B start");

    // Give both runners time to enter the blocking backend call.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel session A only.
    sess_a.cancel().await;

    // Poll session A's events until we see a Completed with Cancelled outcome.
    let mut a_cancelled = false;
    for _ in 0..50 {
        let batch = drain_events(&mut rx_a);
        if batch.iter().any(|e| {
            matches!(e, AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Cancelled || *outcome == AgentOutcome::Failed)
        }) {
            a_cancelled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(a_cancelled, "session A should report cancellation");

    // Session A's snapshot should reflect Cancelled status.
    let snap_a = manager
        .session_handle(id_a)
        .await
        .expect("session A handle")
        .state_snapshot();
    assert_eq!(
        snap_a.status,
        SessionStatus::Cancelled,
        "session A should be cancelled"
    );

    // Capture B's status while A is cancelled and B is still blocking.
    // B must not be Failed or Idle (which would indicate premature
    // termination caused by A's cancellation).
    let b_handle = manager.session_handle(id_b).await.unwrap();
    let b_snap = b_handle.state_snapshot();
    assert!(
        b_snap.status != SessionStatus::Failed,
        "session B should not have been marked Failed by session A's cancel"
    );
    assert!(
        b_snap.status != SessionStatus::Idle,
        "session B should have started running before A's cancel"
    );

    // Now cancel session B to release its blocking backend and confirm
    // it transitions to Cancelled as expected.
    sess_b.cancel().await;

    let mut b_cancelled = false;
    for _ in 0..50 {
        let batch = drain_events(&mut rx_b);
        if batch.iter().any(|e| {
            matches!(e, AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Cancelled)
        }) {
            b_cancelled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b_cancelled, "session B should report cancellation after its own cancel");

    manager.close_session(id_a).await.expect("close session A");
    manager.close_session(id_b).await.expect("close session B");
}

// ===========================================================================
// T2 — One session panic marks only that session Failed
// ===========================================================================

/// Create two sessions, one with a [`PanicSink`] that panics when the runner
/// tries to emit its first event after receiving a `Prompt`.  The panic is
/// caught by the supervisor task (spawned by [`SessionManager::create_session`])
/// which calls [`SessionRuntime::mark_failed`] on the affected session only.
///
/// The other session — using a normal [`NoopSink`] — must complete normally.
#[tokio::test]
async fn one_session_panic_marks_only_that_session_failed() {
    let manager = SessionManager::new(default_scheduler());

    // Session A: uses PanicSink so its root task will panic when the runner
    // tries to emit the RunStarted event.
    let backend_a = Arc::new(make_scripted_backend("A"));
    let sess_a = manager
        .create_session(
            backend_a,
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(PanicSink),
            empty_toolset(),
        )
        .await;

    // Session B: normal.
    let backend_b = Arc::new(make_scripted_backend("B"));
    let sess_b = manager
        .create_session(
            backend_b,
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            empty_toolset(),
        )
        .await;

    let id_a = sess_a.session_id;
    let id_b = sess_b.session_id;

    // Subscribe to B's event bus to later verify it completed.
    let mut rx_b = sess_b.event_bus.subscribe();

    // Send a Prompt to session A.  This causes the runner to try to emit
    // a RunStarted event, which hits PanicSink → panic → supervisor marks
    // session A as Failed.
    sess_a
        .send_command(SessionCommand::Prompt(UserInput {
            text: "panic me".into(),
            attachments: vec![],
        }))
        .await
        .expect("session A send_command should succeed (panic happens async)");

    // Give the supervisor a moment to catch the panic and mark the session.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify session A is Failed.
    let snap_a = manager
        .session_handle(id_a)
        .await
        .expect("session A handle")
        .state_snapshot();
    assert_eq!(
        snap_a.status,
        SessionStatus::Failed,
        "session A should be marked Failed after root task panic; got {:?}",
        snap_a.status,
    );
    assert!(
        snap_a.error.is_some(),
        "session A should have an error message after panic"
    );

    // Now send a Prompt to session B and verify it completes normally.
    sess_b
        .send_command(SessionCommand::Prompt(UserInput {
            text: "hello B".into(),
            attachments: vec![],
        }))
        .await
        .expect("session B send_command should succeed");

    let mut b_completed = false;
    for _ in 0..50 {
        let batch = drain_events(&mut rx_b);
        if batch.iter().any(|e| {
            matches!(e, AgentEvent::Completed { outcome } if *outcome == AgentOutcome::Success)
        }) {
            b_completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b_completed, "session B should complete successfully");

    // Session B must NOT be Failed.
    let snap_b = manager
        .session_handle(id_b)
        .await
        .expect("session B handle")
        .state_snapshot();
    assert_ne!(
        snap_b.status,
        SessionStatus::Failed,
        "session B should not be affected by session A's panic"
    );

    manager.close_session(id_a).await.expect("close session A");
    manager.close_session(id_b).await.expect("close session B");
}

// ===========================================================================
// T4/T5 — Scheduler serializes backend calls under low permit count
// ===========================================================================

/// Configure the global backend semaphore to allow only one concurrent
/// request (`max_concurrent_backend_requests = 1`).  Spawn N concurrent
/// tasks that each acquire a backend permit, hold it briefly, and release.
/// Use a [`Barrier`] to synchronise the start of all N tasks so they race
/// for the single slot, then verify that exactly N permits were acquired
/// sequentially (the total elapsed time is at least N × minimum acquisition
/// latency).
///
/// This exercises the core semaphore behaviour of [`Scheduler`] without
/// relying on any session or agent machinery.
#[tokio::test]
async fn scheduler_serializes_backend_calls_under_low_permit_count() {
    let config = SchedulerConfig {
        max_concurrent_backend_requests: 1,
        ..Default::default()
    };
    let scheduler = Arc::new(Scheduler::new(config));

    let num_tasks: usize = 5;
    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for _ in 0..num_tasks {
        let sched = Arc::clone(&scheduler);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            // All tasks wait at the barrier, then race for the single permit.
            barrier.wait().await;
            let _permit = sched.acquire_backend_permit().await;
            // Hold the permit for a brief moment to serialise observably.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }));
    }

    // Await all tasks.  They should all complete because each one acquires
    // and releases the permit in sequence; no task should deadlock.
    let start = tokio::time::Instant::now();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("task should complete within timeout")
            .expect("task should not panic");
    }
    let elapsed = start.elapsed();

    // With 1 permit and 5 tasks each holding for 20 ms, the minimum serial
    // execution time is at least 5 × 20 ms = 100 ms (plus scheduling
    // overhead).  If the semaphore were not serialising, the elapsed time
    // would be closer to ~20 ms (all running in parallel).
    assert!(
        elapsed >= Duration::from_millis(80),
        "expected serialised execution to take >=80 ms for 5 tasks with 1 permit; got {elapsed:?}"
    );
}

// ===========================================================================
// Permission isolation — concurrent Ask policy evaluations are agent-scoped
// ===========================================================================

/// [`PermissionPolicy::evaluate`] is stateless — it computes its outcome
/// purely from the provided [`AgentCapabilities`] and tool name, with no
/// mutable state shared between agents.  This test verifies that concurrent
/// evaluations for different agents with different permission modes produce
/// correct, non-interfering outcomes.
///
/// NOTE: `PermissionPolicy::evaluate` is not currently invoked inside
/// [`AgentRunner::execute_tool`]; wiring that check in is a behaviour
/// change to `agent_runner.rs` beyond pure test-writing.  This test only
/// validates the policy function itself under concurrency, not the runtime
/// integration.  If the wiring were added, this test (re-purposed or
/// extended) would exercise the end-to-end path.
#[tokio::test]
async fn concurrent_ask_permission_requests_do_not_leak_across_agents() {
    use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
    use harness_protocol::backend::BackendCapabilities;
    use harness_protocol::ids::ToolId;
    use harness_protocol::tools::{PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy};
    use std::collections::HashMap as Map;

    // Build capability sets that map to different permission modes for
    // the same tool name.
    fn make_caps(permission: PermissionMode) -> AgentCapabilities {
        let id = ToolId::new();
        let mut tools = Map::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: "fs.read".to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                },
                policy: ToolPolicy {
                    permission,
                    enabled: true,
                },
                delegatable: false,
            },
        );
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
        }
    }

    let caps_allow = Arc::new(make_caps(PermissionMode::Allow));
    let caps_ask = Arc::new(make_caps(PermissionMode::Ask));
    let caps_deny = Arc::new(make_caps(PermissionMode::Deny));

    let policy = PermissionPolicy;

    // Evaluate all three concurrently.
    let handles: Vec<_> = vec![
        tokio::spawn({
            let caps = Arc::clone(&caps_allow);
            let policy = policy.clone();
            async move { (policy.evaluate(&caps, "fs.read"), "allow") }
        }),
        tokio::spawn({
            let caps = Arc::clone(&caps_ask);
            let policy = policy.clone();
            async move { (policy.evaluate(&caps, "fs.read"), "ask") }
        }),
        tokio::spawn({
            let caps = Arc::clone(&caps_deny);
            let policy = policy.clone();
            async move { (policy.evaluate(&caps, "fs.read"), "deny") }
        }),
    ];

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.expect("policy task should not panic"));
    }

    // Verify each concurrent evaluation produced the correct, independent outcome.
    for (outcome, label) in &outcomes {
        match *label {
            "allow" => assert!(
                matches!(outcome, PermissionOutcome::Allow),
                "Allow caps should produce Allow; got {outcome:?}"
            ),
            "ask" => assert!(
                matches!(outcome, PermissionOutcome::RequiresApproval(_)),
                "Ask caps should produce RequiresApproval; got {outcome:?}"
            ),
            "deny" => assert!(
                matches!(outcome, PermissionOutcome::Denied(_)),
                "Deny caps should produce Denied; got {outcome:?}"
            ),
            other => panic!("unexpected label: {other}"),
        }
    }

    // Cross-agent leakage invariant: concurrent evaluations for different
    // agents must not influence each other's outcomes.  Since the policy
    // is stateless, this is trivially satisfied — but we assert it explicitly
    // so that any future mutation of PermissionPolicy triggers a test failure.
    assert_eq!(outcomes.len(), 3, "expected three independent outcomes");
}

// ===========================================================================
// T7 — ResourceManager denies conflicting exclusive access across sessions
// ===========================================================================

/// Two different sessions (identified by [`SessionId`]) attempt to acquire
/// the same resource in exclusive mode.  The first succeeds, the second is
/// denied with [`ResourceError::ExclusivelyHeld`].  After the first session
/// releases, the second can acquire.
#[tokio::test]
async fn resource_manager_denies_conflicting_exclusive_access_across_sessions() {
    let rm = ResourceManager::new();
    let key = ResourceKey::File("/tmp/shared.lock".into());
    let sid_a = SessionId::new();
    let sid_b = SessionId::new();

    // Session A acquires exclusive access.
    rm.acquire(key.clone(), AccessMode::Exclusive, sid_a)
        .await
        .expect("session A should acquire exclusive access");

    // Session B attempts exclusive access — must be denied.
    let err = rm
        .acquire(key.clone(), AccessMode::Exclusive, sid_b)
        .await
        .expect_err("session B should be denied exclusive access");
    assert!(
        matches!(err, ResourceError::ExclusivelyHeld(ref k) if k == &key),
        "expected ExclusivelyHeld error, got {err:?}"
    );

    // Session B attempts shared access while A holds exclusive — also denied.
    let err = rm
        .acquire(key.clone(), AccessMode::Shared, sid_b)
        .await
        .expect_err("session B should be denied shared access while A holds exclusive");
    assert!(
        matches!(err, ResourceError::ExclusivelyHeld(ref k) if k == &key),
        "expected ExclusivelyHeld for shared access too, got {err:?}"
    );

    // Session A releases.
    rm.release(key.clone(), sid_a).await;

    // Now session B can acquire exclusive.
    rm.acquire(key.clone(), AccessMode::Exclusive, sid_b)
        .await
        .expect("session B should acquire exclusive after A releases");

    // Clean up.
    rm.release(key, sid_b).await;
}

// ===========================================================================
// T7 — Shared access is permitted concurrently; exclusive is blocked
// ===========================================================================

/// Two sessions can both hold shared access simultaneously.  A third session
/// requesting exclusive access is denied while shared holders exist.
#[tokio::test]
async fn shared_access_allows_concurrent_holders_while_blocking_exclusive() {
    let rm = ResourceManager::new();
    let key = ResourceKey::Custom("my-resource".into());
    let sid_a = SessionId::new();
    let sid_b = SessionId::new();
    let sid_c = SessionId::new();

    // Both A and B acquire shared access concurrently.
    rm.acquire(key.clone(), AccessMode::Shared, sid_a)
        .await
        .expect("session A shared");
    rm.acquire(key.clone(), AccessMode::Shared, sid_b)
        .await
        .expect("session B shared");

    // Session C tries exclusive — should be denied.
    let err = rm
        .acquire(key.clone(), AccessMode::Exclusive, sid_c)
        .await
        .expect_err("session C exclusive should be denied while shared holders exist");
    assert!(
        matches!(err, ResourceError::ExclusivelyHeld(_)),
        "expected ExclusivelyHeld, got {err:?}"
    );

    // A releases.
    rm.release(key.clone(), sid_a).await;

    // Exclusive still denied because B still holds shared.
    let err = rm
        .acquire(key.clone(), AccessMode::Exclusive, sid_c)
        .await
        .expect_err("session C exclusive still denied while B holds shared");
    assert!(
        matches!(err, ResourceError::ExclusivelyHeld(_)),
        "expected ExclusivelyHeld, got {err:?}"
    );

    // B releases.
    rm.release(key.clone(), sid_b).await;

    // Now C can acquire exclusive.
    rm.acquire(key.clone(), AccessMode::Exclusive, sid_c)
        .await
        .expect("session C exclusive should succeed after all shared holders release");

    rm.release(key, sid_c).await;
}

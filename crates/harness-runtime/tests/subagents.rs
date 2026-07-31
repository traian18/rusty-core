//! Sub-agent integration tests for the harness runtime.
//!
//! These tests exercise the hierarchical [`AgentSupervisor`] through its public
//! [`AgentSupervisor::spawn_and_drive`] entry point, following the same
//! conventions as `tests/concurrency.rs`: deterministic channel/event
//! synchronisation, polling-with-timeout, and the fake backend / tool /
//! workspace test doubles from `harness_runtime::testing` and
//! `harness_runtime::workspace`.
//!
//! # Organisation
//!
//! | Test | Underlying invariant |
//! |------|---------------------|
//! | `test_root_spawns_two_concurrent_children` | A root can spawn two children with `SpawnMode::Concurrent` without blocking; both complete and report to the root's mailbox |
//! | `test_heterogeneous_backends` | A child spawned via `BackendPolicy::Explicit(reference)` executes against its own registered backend, not the parent's (requires T6.6 backend resolution) |
//!
//! The child command senders are read through the supervisor's public
//! [`AgentSupervisor::child_commands`] accessor so the tests can drive each
//! child's run loop with `StartRun` against its scripted backend. Completion is
//! observed explicitly: the child's spawned completion task delivers
//! `AgentCommand::ChildCompleted` into the root's `parent_commands_tx` mailbox,
//! and the child's streamed events arrive on a [`RecordingSink`]. No test relies
//! on raw wall-clock sleeps for its assertions — every wait is a
//! poll-with-timeout on a channel or the recorded event stream.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};

use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionEvent,
    ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentResult, AgentStatus, UserInput};
use harness_protocol::effects::{
    BackendPolicy, SpawnAgentSpec, SpawnMode, ToolInheritance, WorkspacePolicy,
};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, SessionId,
};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;
use serde_json::Value;
use tokio::sync::mpsc;

use harness_runtime::agent_supervisor::{AgentSupervisor, SpawnOutcome};
use harness_runtime::cancellation::SessionCancellation;
use harness_runtime::integration::{IntegrationFactory, IntegrationRegistry};
use harness_runtime::scheduler::{Scheduler, SchedulerConfig};
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A thread-safe [`EventSink`] that records every envelope it receives so
/// tests can assert on the ordered event stream emitted by child runners.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<AgentEventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        self.events.lock().unwrap().push(envelope);
    }
}

/// Builds a root agent capable of spawning children (empty toolset, default
/// budget so the `max_children`/`max_depth` gate is open, spawning enabled).
fn test_parent(session_id: SessionId) -> Agent {
    Agent::new(
        AgentId::new(),
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
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "root-fake".into(),
                description: "root backend".into(),
                capabilities: BackendCapabilities::default(),
            },
        },
        AgentCapabilities {
            tools: AgentToolset {
                tools: HashMap::new(),
            },
            can_spawn_agents: true,
            max_child_depth: Some(5),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: true,
                can_search: true,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

/// A scripted backend that streams a single distinguishable text delta and
/// then completes, so tests can tell which backend (and thus which child) a
/// streamed event came from.
fn scripted_backend(text: &str) -> FakeBackend {
    let request_id = RequestId::new();
    FakeBackend::new()
        .with_events(vec![ExecutionEvent::TextDelta {
            request_id,
            delta: text.to_string(),
        }])
        .with_result(ExecutionResult {
            request_id,
            usage: Default::default(),
            cost: Default::default(),
            finish_reason: "end_turn".into(),
        })
}

/// Polls `sink` (with timeout) until a recorded envelope matches `predicate`.
///
/// Returns `true` if the predicate became true within the window. This is the
/// deterministic, jitter-tolerant replacement for a raw sleep: we never assert
/// "give it N ms and it will be there"; we wait on the event stream itself.
async fn sink_eventually(sink: &RecordingSink, predicate: impl Fn(&AgentEvent) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let events = sink.events.lock().unwrap();
            if events.iter().any(|env| predicate(&env.event)) {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Sends a `StartRun` to the given spawned child so its runner executes its
/// scripted backend. The child's command sender is read through the
/// supervisor's public [`AgentSupervisor::child_commands`] accessor (the child
/// is tracked there immediately after `spawn_and_drive` returns `Detached`).
async fn drive_child(supervisor: &AgentSupervisor, child_id: AgentId) {
    let commands = supervisor
        .child_commands(child_id)
        .await
        .expect("spawned child must be tracked by the supervisor");
    commands
        .send(AgentCommand::StartRun {
            input: UserInput {
                text: "go".into(),
                attachments: vec![],
            },
        })
        .await
        .expect("send StartRun to child");
}

/// A spawn spec with the given role/backend policy, run concurrently.
fn concurrent_spec(
    role: &str,
    backend: BackendPolicy,
    tools: ToolInheritance,
    workspace: WorkspacePolicy,
) -> SpawnAgentSpec {
    SpawnAgentSpec {
        role: Some(role.into()),
        backend,
        tools,
        workspace,
        budget: AgentBudget::default(),
        mode: SpawnMode::Concurrent,
    }
}

/// A test integration factory that returns a fresh [`FakeBackend`] with the
/// scripted behavior configured at construction. Used to register a "second"
/// backend that an explicit child references.
struct FakeIntegrationFactory {
    id: &'static str,
    backend: FakeBackend,
}

#[async_trait]
impl IntegrationFactory for FakeIntegrationFactory {
    fn id(&self) -> &'static str {
        self.id
    }

    fn descriptor(&self) -> BackendDescriptor {
        self.backend.descriptor()
    }

    async fn create(
        &self,
        _config: Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Arc::new(self.backend.clone()))
    }
}

/// A stable integration identifier (UUID string) under which the explicit-child
/// factory is registered in the integration registry.
const CHILD_FAKE_INTEGRATION: &str = "00000000-0000-0000-0000-00000000c0de";

// ===========================================================================
// Root spawns two concurrent children
// ===========================================================================

/// The root agent spawns two children via `spawn_and_drive(SpawnMode::Concurrent)`.
/// Both `spawn_and_drive` calls return `SpawnOutcome::Detached` immediately —
/// the root is never blocked on either child — and both children then stream
/// their scripted [`FakeBackend`] events concurrently, each completing in the
/// background and delivering its real `ChildCompleted` into the root's mailbox.
///
/// Synchronisation is explicit throughout:
///   * the root's mailbox (`parent_commands_rx`) delivers each child's
///     `ChildCompleted` — we await exactly two of them with a timeout, never a
///     sleep;
///   * the child runners' events are observed through a shared
///     [`RecordingSink`], polled with a timeout for each child's distinguishing
///     text delta.
#[tokio::test]
async fn test_root_spawns_two_concurrent_children() {
    let session_id = SessionId::new();
    let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());
    let parent = test_parent(session_id);

    // Two distinguishable scripted backends so we can attribute each streamed
    // event to a specific child.
    let backend_a: Arc<dyn ExecutionBackend> = Arc::new(scripted_backend("child-A-work"));
    let backend_b: Arc<dyn ExecutionBackend> = Arc::new(scripted_backend("child-B-work"));

    let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
    let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
    let recording = Arc::new(RecordingSink::default());
    let event_sink: Arc<dyn EventSink> = recording.clone();
    let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
    let integration_registry = IntegrationRegistry::new();
    let (parent_commands_tx, mut parent_commands_rx) = mpsc::channel::<AgentCommand>(64);

    // Spawn child A (Concurrent — must return immediately).
    let outcome_a = supervisor
        .spawn_and_drive(
            &parent,
            backend_a,
            &integration_registry,
            Arc::clone(&tool_registry),
            Arc::clone(&workspace),
            Arc::clone(&event_sink),
            Arc::clone(&scheduler),
            parent_commands_tx.clone(),
            concurrent_spec(
                "child-A",
                BackendPolicy::Inherit,
                ToolInheritance::InheritAll,
                WorkspacePolicy::Inherit,
            ),
        )
        .await
        .expect("root should spawn child A");
    let child_a = match outcome_a {
        SpawnOutcome::Detached(id) => id,
        other => panic!("expected Detached for child A, got {other:?}"),
    };

    // Spawn child B (Concurrent — must return immediately, root not blocked).
    let outcome_b = supervisor
        .spawn_and_drive(
            &parent,
            backend_b,
            &integration_registry,
            tool_registry,
            workspace,
            event_sink,
            scheduler,
            parent_commands_tx,
            concurrent_spec(
                "child-B",
                BackendPolicy::Inherit,
                ToolInheritance::InheritAll,
                WorkspacePolicy::Inherit,
            ),
        )
        .await
        .expect("root should spawn child B");
    let child_b = match outcome_b {
        SpawnOutcome::Detached(id) => id,
        other => panic!("expected Detached for child B, got {other:?}"),
    };

    // Both spawns were Concurrent → neither blocked the root; both children are
    // now live in the background. Drive each to actually run its backend.
    drive_child(&supervisor, child_a).await;
    drive_child(&supervisor, child_b).await;

    // Both children stream their own distinguishing text delta concurrently.
    assert!(
        sink_eventually(
            &recording,
            |e| matches!(e, AgentEvent::AssistantTextDelta { delta, .. } if delta == "child-A-work")
        )
        .await,
        "child A should stream its own backend text"
    );
    assert!(
        sink_eventually(
            &recording,
            |e| matches!(e, AgentEvent::AssistantTextDelta { delta, .. } if delta == "child-B-work")
        )
        .await,
        "child B should stream its own backend text"
    );

    // Both children complete in the background and report into the root's
    // mailbox. We await exactly two `ChildCompleted` commands (one per child),
    // in whatever order they completed — the mailbox is our explicit
    // synchronisation point, not a sleep.
    let first = tokio::time::timeout(Duration::from_secs(5), parent_commands_rx.recv())
        .await
        .expect("first child should report completion promptly")
        .expect("root mailbox should not close");
    let second = tokio::time::timeout(Duration::from_secs(5), parent_commands_rx.recv())
        .await
        .expect("second child should report completion promptly")
        .expect("root mailbox should not close");

    // Collect the two completions and verify each corresponds to one of the two
    // spawned children.
    let mut completed: Vec<(AgentId, AgentResult)> = Vec::new();
    for command in [first, second] {
        match command {
            AgentCommand::ChildCompleted { agent_id, result } => {
                completed.push((agent_id, result));
            }
            other => panic!("expected ChildCompleted, got {other:?}"),
        }
    }
    let ids: Vec<AgentId> = completed.iter().map(|(id, _)| *id).collect();
    assert!(
        ids.contains(&child_a) && ids.contains(&child_b),
        "both spawned children must complete and report to the root, got {ids:?}"
    );

    // Applying each completion to the root yields the ordered `ChildAgentCompleted`
    // markers — the root observes both children complete, in the same order the
    // mailbox delivered them.
    let mut root = parent.clone();
    let mut markers = Vec::new();
    for (agent_id, result) in completed {
        let effects = root.apply(AgentCommand::ChildCompleted { agent_id, result });
        for effect in effects {
            if let harness_protocol::effects::AgentEffect::Emit {
                event: AgentEvent::ChildAgentCompleted { agent_id, outcome },
            } = effect
            {
                markers.push((agent_id, outcome));
            }
        }
    }
    assert_eq!(
        markers
            .iter()
            .filter(|(_, o)| *o == AgentOutcome::Success)
            .count(),
        2,
        "root should observe both children complete successfully"
    );

    // Both children also emitted their terminal `Completed` event on the shared
    // event stream (agent added → text → completed), independently of the root.
    assert!(
        sink_eventually(&recording, |e| matches!(e, AgentEvent::Completed { .. })).await,
        "both children should emit a terminal Completed event"
    );

    // The root was never blocked: `spawn_and_drive` returned immediately with
    // both Detached outcomes, and no run was started on the root itself.
    assert_eq!(
        root.state.status,
        AgentStatus::Idle,
        "root stays Idle (never started a run)"
    );
}

// ===========================================================================
// Heterogeneous backends (root vs. explicit child backend)
// ===========================================================================

/// The root is bound to one [`FakeBackend`]; a child is spawned with
/// `BackendPolicy::Explicit(reference)` pointing at a **second** registered
/// backend. The child must execute against its own backend's scripted events,
/// never the parent's.
///
/// NOTE: This is the capstone of T6.6 (`BackendPolicy::Explicit` resolution via
/// the [`IntegrationRegistry`]). Until that resolution is wired into
/// `AgentSupervisor::spawn_child`, `BackendPolicy::Explicit` falls back to the
/// parent backend, so this test is gated on T6.6.
#[tokio::test]
async fn test_heterogeneous_backends() {
    let session_id = SessionId::new();
    let supervisor = AgentSupervisor::new(session_id, SessionCancellation::new());
    let parent = test_parent(session_id);

    // The root's own backend streams a distinctive marker the child must NOT use.
    let root_marker = "root-backend";
    let root_backend: Arc<dyn ExecutionBackend> = Arc::new(scripted_backend(root_marker));

    // The second (explicitly referenced) backend streams a different marker.
    let child_marker = "child-own-backend";
    let child_backend = scripted_backend(child_marker);

    let integration_registry = IntegrationRegistry::new();
    integration_registry
        .register(Arc::new(FakeIntegrationFactory {
            id: CHILD_FAKE_INTEGRATION,
            backend: child_backend,
        }))
        .expect("register explicit-child factory");

    let tool_registry: Arc<dyn ToolRegistry> = Arc::new(FakeToolRegistry::new());
    let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
    let recording = Arc::new(RecordingSink::default());
    let event_sink: Arc<dyn EventSink> = recording.clone();
    let scheduler: Arc<Scheduler> = Arc::new(Scheduler::new(SchedulerConfig::default()));
    let (parent_commands_tx, mut parent_commands_rx) = mpsc::channel::<AgentCommand>(64);

    // Reference the second backend by its registered integration id.
    let child_reference = BackendReference {
        integration: CHILD_FAKE_INTEGRATION
            .parse()
            .expect("valid integration id"),
        configuration: ConfigurationId::new(),
        model: None,
    };

    let outcome = supervisor
        .spawn_and_drive(
            &parent,
            root_backend,
            &integration_registry,
            tool_registry,
            workspace,
            event_sink,
            scheduler,
            parent_commands_tx,
            concurrent_spec(
                "child",
                BackendPolicy::Explicit(child_reference),
                ToolInheritance::InheritAll,
                WorkspacePolicy::Inherit,
            ),
        )
        .await
        .expect("root should spawn the explicit-backend child");

    let child_id = match outcome {
        SpawnOutcome::Detached(id) => id,
        other => panic!("expected Detached, got {other:?}"),
    };

    // Drive the child: it must execute against the explicitly referenced
    // backend, streaming that backend's marker.
    drive_child(&supervisor, child_id).await;

    assert!(
        sink_eventually(
            &recording,
            |e| matches!(e, AgentEvent::AssistantTextDelta { delta, .. } if delta == child_marker)
        )
        .await,
        "child should stream events from its own explicitly referenced backend"
    );

    // The parent backend's marker must never surface from the child. The lock
    // is scoped so it is released before the next await below.
    {
        let events = recording.events.lock().unwrap();
        assert!(
            !events.iter().any(|env| matches!(
                &env.event,
                AgentEvent::AssistantTextDelta { delta, .. } if delta == root_marker
            )),
            "child must NOT execute against the parent's backend"
        );
    }

    // And the child completes, reporting back to the root's mailbox.
    let command = tokio::time::timeout(Duration::from_secs(5), parent_commands_rx.recv())
        .await
        .expect("child should report completion promptly")
        .expect("root mailbox should not close");
    assert!(
        matches!(&command, AgentCommand::ChildCompleted { agent_id, .. } if *agent_id == child_id),
        "root should receive ChildCompleted for the explicit child, got {command:?}"
    );
}

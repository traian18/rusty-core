//! End-to-end subagent spawning, result delivery, and cancellation tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendReference, ExecutionEvent, ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentStatus, UserInput};
use harness_protocol::effects::{
    BackendPolicy, SpawnAgentSpec, SpawnMode, ToolInheritance, WorkspacePolicy,
};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
use harness_protocol::ids::{AgentId, ConfigurationId, IntegrationId, RequestId, SessionId};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::AgentBudget;
use harness_runtime::agent_runner::{AgentRunner, AgentTask};
use harness_runtime::agent_supervisor::AgentSupervisor;
use harness_runtime::cancellation::SessionCancellation;
use harness_runtime::integration::IntegrationRegistry;
use harness_runtime::scheduler::{Scheduler, SchedulerConfig};
use harness_runtime::session_runtime::LiveStateTable;
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<AgentEventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn send(&self, event: AgentEventEnvelope) {
        self.events.lock().expect("event lock poisoned").push(event);
    }
}

fn root_agent(session_id: SessionId, agent_id: AgentId, backend: &dyn ExecutionBackend) -> Agent {
    Agent::new(
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
            tools: AgentToolset {
                tools: HashMap::new(),
            },
            can_spawn_agents: true,
            max_child_depth: Some(4),
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: true,
                can_search: true,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget {
            max_children: Some(2),
            max_depth: Some(4),
            ..Default::default()
        },
    )
}

fn child_spec(role: &str) -> SpawnAgentSpec {
    SpawnAgentSpec {
        role: Some(role.into()),
        backend: BackendPolicy::Inherit,
        tools: ToolInheritance::InheritAll,
        workspace: WorkspacePolicy::Inherit,
        budget: AgentBudget {
            max_children: Some(0),
            max_depth: Some(0),
            ..Default::default()
        },
        mode: SpawnMode::Concurrent,
    }
}

async fn wait_for_spawned(sink: &RecordingSink, count: usize) -> Vec<AgentId> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ids = sink
            .events
            .lock()
            .expect("event lock poisoned")
            .iter()
            .filter_map(|envelope| match envelope.event {
                AgentEvent::ChildAgentSpawned { agent_id } => Some(agent_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        if ids.len() >= count {
            return ids;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "children were not spawned"
        );
        tokio::task::yield_now().await;
    }
}

async fn wait_for_parent_completions(sink: &RecordingSink, parent_id: AgentId, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let completed = sink
            .events
            .lock()
            .expect("event lock poisoned")
            .iter()
            .filter(|envelope| {
                envelope.agent_id == parent_id
                    && matches!(envelope.event, AgentEvent::ChildAgentCompleted { .. })
            })
            .count();
        if completed >= count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "parent did not receive both child results"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn runner_interprets_two_spawn_effects_and_receives_both_results() {
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
    let root = root_agent(session_id, root_id, backend.as_ref());
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

    commands
        .send(AgentCommand::SpawnChild {
            spec: child_spec("child-a"),
        })
        .await
        .expect("spawn child a");
    commands
        .send(AgentCommand::SpawnChild {
            spec: child_spec("child-b"),
        })
        .await
        .expect("spawn child b");

    let child_ids = wait_for_spawned(&sink, 2).await;
    assert_ne!(child_ids[0], child_ids[1]);

    // Concurrent mode leaves the parent command loop live while both children
    // are pending.
    commands
        .send(AgentCommand::Pause)
        .await
        .expect("pause root");
    commands
        .send(AgentCommand::Resume)
        .await
        .expect("resume root");

    for child_id in child_ids {
        supervisor
            .child_commands(child_id)
            .await
            .expect("registered child mailbox")
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "execute".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("start child");
    }

    wait_for_parent_completions(&sink, root_id, 2).await;

    let parent_transitions = sink
        .events
        .lock()
        .expect("event lock poisoned")
        .iter()
        .filter(|event| event.agent_id == root_id)
        .filter_map(|event| match event.event {
            AgentEvent::StateChanged { to, .. } => Some(to),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(parent_transitions.contains(&AgentStatus::Paused));
    assert!(parent_transitions.contains(&AgentStatus::Idle));

    root_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), runner_task)
        .await
        .expect("root runner stops after cancellation")
        .expect("root runner does not panic");
}

#[test]
fn cancellation_token_tree_is_parent_scoped() {
    let session = CancellationToken::new();
    let root = session.child_token();
    let child = root.child_token();
    let sibling_root = session.child_token();

    root.cancel();

    assert!(child.is_cancelled());
    assert!(!sibling_root.is_cancelled());
}

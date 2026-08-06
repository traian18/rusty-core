//! RC-203 deterministic race certification for the agent transition core.

use std::collections::HashMap;

use harness_core::agent::Agent;
use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use harness_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference, ExecutionError,
    ExecutionEvent, ExecutionResult,
};
use harness_protocol::commands::{AgentCommand, AgentStatus, UserInput};
use harness_protocol::effects::AgentEffect;
use harness_protocol::ids::{
    AgentId, BackendId, ConfigurationId, IntegrationId, RequestId, SessionId,
};
use harness_protocol::messages::{ContentBlock, MessageRole};
use harness_protocol::tools::AgentToolset;
use harness_protocol::usage::{AgentBudget, Cost, ModelUsage, UsageValue};

fn agent() -> Agent {
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        "system".into(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "race-certification-backend".into(),
                description: "deterministic test backend".into(),
                capabilities: BackendCapabilities::default(),
            },
        },
        AgentCapabilities {
            tools: AgentToolset {
                tools: HashMap::new(),
            },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: true,
                can_write: false,
                can_search: true,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

fn input(text: &str) -> UserInput {
    UserInput {
        text: text.into(),
        attachments: vec![],
    }
}

fn completed() -> ExecutionResult {
    ExecutionResult {
        request_id: RequestId::new(),
        usage: ModelUsage {
            input_tokens: UsageValue::new(Some(1)),
            output_tokens: UsageValue::new(Some(2)),
            total_tokens: UsageValue::new(Some(3)),
            ..Default::default()
        },
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    }
}

fn start(agent: &mut Agent, text: &str) -> harness_protocol::ids::RunId {
    let effects = agent.apply(AgentCommand::StartRun {
        input: input(text),
    });
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::ExecuteBackend { .. }
    )));
    agent.state.active_run.expect("run should be active")
}

fn finish(agent: &mut Agent, run_id: harness_protocol::ids::RunId) {
    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed(),
        },
    });
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::FinishRun { .. }
    )));
}

#[test]
fn three_sequential_prompts_preserve_transcript_order() {
    let mut agent = agent();

    let first = start(&mut agent, "one");
    finish(&mut agent, first);

    let second = start(&mut agent, "two");
    finish(&mut agent, second);

    let third = start(&mut agent, "three");
    finish(&mut agent, third);

    let prompts: Vec<_> = agent
        .state
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .map(|message| match &message.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            _ => panic!("prompt must be text"),
        })
        .collect();

    assert_eq!(prompts, ["one", "two", "three"]);
}

#[test]
fn multiple_follow_ups_are_fifo_and_survive_cancellation() {
    let mut agent = agent();
    let first = start(&mut agent, "first");

    for text in ["second", "third", "fourth"] {
        assert!(agent
            .apply(AgentCommand::FollowUp { input: input(text) })
            .is_empty());
    }

    agent.apply(AgentCommand::Cancel);
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert_eq!(agent.state.queued_inputs.len(), 3);

    for expected in ["second", "third", "fourth"] {
        let effects = agent.apply(AgentCommand::StartNextQueuedRun);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            AgentEffect::ExecuteBackend { request }
                if matches!(
                    request.messages.last().map(|message| &message.content[0]),
                    Some(ContentBlock::Text { text }) if text == expected
                )
        )));
        let run_id = agent.state.active_run.expect("queued run should start");
        assert_ne!(run_id, first);
        finish(&mut agent, run_id);
    }

    assert!(agent.state.queued_inputs.is_empty());
}

#[test]
fn cancellation_before_backend_start_has_no_late_completion_effect() {
    let mut agent = agent();
    let run_id = start(&mut agent, "cancel before start");

    let cancel_effects = agent.apply(AgentCommand::Cancel);
    assert!(cancel_effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::CancelBackend { run_id: cancelled } if *cancelled == run_id
    )));
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    assert!(agent.state.active_run.is_none());

    let late = agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::Completed {
            request_id: RequestId::new(),
            result: completed(),
        },
    });
    assert!(late.is_empty());
    assert_eq!(agent.state.status, AgentStatus::Cancelled);
}

#[test]
fn cancellation_while_streaming_suppresses_late_deltas_and_errors() {
    let mut agent = agent();
    let run_id = start(&mut agent, "streaming");
    agent.apply(AgentCommand::BackendEvent {
        run_id,
        event: ExecutionEvent::TextDelta {
            request_id: RequestId::new(),
            delta: "partial".into(),
        },
    });
    assert_eq!(agent.state.status, AgentStatus::Streaming);

    agent.apply(AgentCommand::Cancel);

    for event in [
        ExecutionEvent::TextDelta {
            request_id: RequestId::new(),
            delta: "late".into(),
        },
        ExecutionEvent::Error {
            request_id: RequestId::new(),
            error: ExecutionError::Timeout,
        },
    ] {
        assert!(agent
            .apply(AgentCommand::BackendEvent { run_id, event })
            .is_empty());
    }

    assert_eq!(agent.state.status, AgentStatus::Cancelled);
    let assistant_text: String = agent
        .state
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(assistant_text, "partial");
}

#[test]
fn failed_run_can_be_followed_by_a_new_prompt() {
    let mut agent = agent();
    let failed_run = start(&mut agent, "will fail");

    let effects = agent.apply(AgentCommand::BackendEvent {
        run_id: failed_run,
        event: ExecutionEvent::Error {
            request_id: RequestId::new(),
            error: ExecutionError::BackendError {
                message: "temporary".into(),
                code: "TEMPORARY".into(),
            },
        },
    });
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AgentEffect::FinishRun { .. }
    )));
    assert_eq!(agent.state.status, AgentStatus::Failed);
    assert!(agent.state.active_run.is_none());

    let next = agent.apply(AgentCommand::StartRun {
        input: input("recover"),
    });
    assert!(next.iter().any(|effect| matches!(
        effect,
        AgentEffect::ExecuteBackend { .. }
    )));
    assert!(agent.state.active_run.is_some());
    assert_eq!(agent.state.status, AgentStatus::PreparingContext);
}

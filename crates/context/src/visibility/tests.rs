use super::*;

use rusty_protocol::ids::{RequestId, RunId, Timestamp};
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use crate::testing::{observation, RecordingPublisher};

/// A request on `model` whose text estimates to about `tokens` input tokens.
fn request(model: Option<&str>, tokens: usize) -> ExecutionRequest {
    let mut request = ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: String::new(),
        messages: vec![AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "abcd".repeat(tokens),
            }],
            created_at: Timestamp::now(),
        }],
        tools: vec![],
        extended_thinking: false,
        params: Default::default(),
    };
    request.params.model = model.map(str::to_owned);
    request
}

/// No output reserve: the input budget is 90% of the window.
fn policy() -> ContextPolicy {
    ContextPolicy {
        reserved_output_tokens: 0,
        ..ContextPolicy::default()
    }
}

fn availability_of(request: &ExecutionRequest, window: Option<(&str, u64)>) -> ContextAvailability {
    let observation = observation(request.request_id, window, RecordingPublisher::new());
    match context_update(request, &observation, policy()) {
        AgentEvent::ContextUpdated {
            availability,
            request_start_order,
            compaction_started,
            compaction_ended,
            ..
        } => {
            assert_eq!(request_start_order, 3);
            assert!(!compaction_started && !compaction_ended);
            availability
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn below_the_budget_the_remainder_is_the_budget_minus_the_estimate() {
    let available = availability_of(&request(Some("m"), 400), Some(("m", 1_000)));
    assert_eq!(
        available,
        ContextAvailability::Available {
            estimated_input_tokens: 400,
            input_budget: 900,
            model_window: 1_000,
            remaining_input_tokens: 500,
            usage_percent: 45,
        }
    );
}

#[test]
fn at_or_over_the_budget_nothing_remains() {
    for tokens in [900, 2_000] {
        let ContextAvailability::Available {
            remaining_input_tokens,
            usage_percent,
            ..
        } = availability_of(&request(Some("m"), tokens), Some(("m", 1_000)))
        else {
            panic!("expected an estimate");
        };
        assert_eq!(remaining_input_tokens, 0);
        assert!(usage_percent >= 100);
    }
}

#[test]
fn an_unknown_mismatched_or_unusable_window_is_unavailable() {
    let unavailable = |reason| ContextAvailability::Unavailable { reason };
    assert_eq!(
        availability_of(&request(Some("m"), 10), None),
        unavailable(ContextUnavailableReason::UnknownModelWindow)
    );
    assert_eq!(
        availability_of(&request(Some("other"), 10), Some(("m", 1_000))),
        unavailable(ContextUnavailableReason::ModelWindowMismatch)
    );
    // A request on the backend's default model cannot be matched to the window.
    assert_eq!(
        availability_of(&request(None, 10), Some(("m", 1_000))),
        unavailable(ContextUnavailableReason::ModelWindowMismatch)
    );
    assert_eq!(
        availability_of(&request(Some("m"), 10), Some(("m", 0))),
        unavailable(ContextUnavailableReason::UnusableInputBudget)
    );
}

#[test]
fn the_usage_percentage_saturates() {
    let ContextAvailability::Available { usage_percent, .. } =
        availability_of(&request(Some("m"), 2_000_000), Some(("m", 10)))
    else {
        panic!("expected an estimate");
    };
    assert_eq!(usage_percent, u16::MAX);
}

#[tokio::test]
async fn an_operation_starts_and_finishes_under_one_id_with_truthful_measurements() {
    let publisher = RecordingPublisher::new();
    let observation = observation(RequestId::new(), None, publisher.clone());
    let standard = CompactionOperation::start(&observation, CompactionType::Standard, Some(90))
        .await
        .unwrap();
    standard.finish(CompactionOutcome::Failure, Some(40)).await;
    let summary =
        CompactionOperation::start(&observation, CompactionType::BackgroundSummary, Some(90))
            .await
            .unwrap();
    summary.finish(CompactionOutcome::Success, Some(40)).await;
    let events = publisher.events();
    assert_eq!(events.len(), 4);
    let AgentEvent::CompactionFinished {
        after_estimated_input_tokens,
        before_estimated_input_tokens,
        ..
    } = &events[1]
    else {
        panic!("expected a finish");
    };
    assert_eq!(*before_estimated_input_tokens, Some(90));
    assert_eq!(
        *after_estimated_input_tokens, None,
        "no after count on failure"
    );
    for event in &events[2..] {
        match event {
            AgentEvent::CompactionStarted {
                before_estimated_input_tokens,
                ..
            } => assert_eq!(*before_estimated_input_tokens, None),
            AgentEvent::CompactionFinished {
                before_estimated_input_tokens,
                after_estimated_input_tokens,
                ..
            } => {
                assert_eq!(*before_estimated_input_tokens, None);
                assert_eq!(*after_estimated_input_tokens, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let lifecycle = publisher.lifecycle();
    assert_eq!(lifecycle[0].3, lifecycle[1].3);
    assert_eq!(lifecycle[2].3, lifecycle[3].3);
    assert_ne!(lifecycle[0].3, lifecycle[2].3, "operations are told apart");
}

#[tokio::test]
async fn a_start_that_could_not_be_published_has_no_finish() {
    let publisher = std::sync::Arc::new(RecordingPublisher {
        events: Default::default(),
        accept: false,
    });
    let observation = observation(RequestId::new(), None, publisher);
    assert!(
        CompactionOperation::start(&observation, CompactionType::Standard, None)
            .await
            .is_none()
    );
}

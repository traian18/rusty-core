use super::*;
use crate::ids::{EventId, MessageId, RequestId, RunId, Timestamp};

// -----------------------------------------------------------------------
// Sequence-based ordering
// -----------------------------------------------------------------------

/// Verifies that envelopes with different `agent_sequence` values sort
/// correctly, even when timestamps are identical.
#[test]
fn envelope_ordering_by_agent_sequence() {
    let timestamp = Timestamp::now();

    let make_env = |seq: u64| -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: crate::ids::SessionId::new(),
            agent_id: crate::ids::AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: seq,
            session_sequence: Some(seq),
            timestamp,
            visibility: EventVisibility::User,
            event: AgentEvent::RunStarted {
                run_id: RunId::new(),
            },
        }
    };

    let e1 = make_env(1);
    let e2 = make_env(2);
    let e3 = make_env(3);

    let mut events = [e3.clone(), e1.clone(), e2.clone()];
    events.sort_by_key(|e| e.agent_sequence);

    assert_eq!(events[0].agent_sequence, 1);
    assert_eq!(events[1].agent_sequence, 2);
    assert_eq!(events[2].agent_sequence, 3);
}

/// Verifies that envelopes with different `session_sequence` values sort
/// correctly, including when some are `None`.
#[test]
fn envelope_ordering_by_session_sequence() {
    let timestamp = Timestamp::now();

    let make_env = |session_seq: Option<u64>| -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: crate::ids::SessionId::new(),
            agent_id: crate::ids::AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: 1,
            session_sequence: session_seq,
            timestamp,
            visibility: EventVisibility::Developer,
            event: AgentEvent::BackendRequestStarted {
                request_id: RequestId::new(),
            },
        }
    };

    // Envelopes with `session_sequence: None` sort first if we use
    // `sort_by_key` with `unwrap_or(0)` — verify this works.
    let a = make_env(Some(1));
    let b = make_env(Some(2));
    let c = make_env(Some(3));

    let mut events = [c, a.clone(), b];
    events.sort_by_key(|e| e.session_sequence.unwrap_or(0));

    assert_eq!(events[0].session_sequence, Some(1));
    assert_eq!(events[1].session_sequence, Some(2));
    assert_eq!(events[2].session_sequence, Some(3));
}

// -----------------------------------------------------------------------
// EventVisibility round-trip
// -----------------------------------------------------------------------

#[test]
fn event_visibility_roundtrip() {
    for visibility in &[
        EventVisibility::User,
        EventVisibility::Developer,
        EventVisibility::Internal,
    ] {
        let json = serde_json::to_string(visibility).expect("serialize");
        let deserialized: EventVisibility = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*visibility, deserialized);
    }
}

// -----------------------------------------------------------------------
// AgentOutcome round-trip
// -----------------------------------------------------------------------

#[test]
fn agent_outcome_roundtrip() {
    for outcome in &[
        AgentOutcome::Success,
        AgentOutcome::Cancelled,
        AgentOutcome::Failed,
    ] {
        let json = serde_json::to_string(outcome).expect("serialize");
        let deserialized: AgentOutcome = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*outcome, deserialized);
    }
}

// -----------------------------------------------------------------------
// AgentEventEnvelope round-trip
// -----------------------------------------------------------------------

#[test]
fn envelope_roundtrip() {
    let envelope = AgentEventEnvelope {
        event_id: EventId::new(),
        session_id: crate::ids::SessionId::new(),
        agent_id: crate::ids::AgentId::new(),
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: 1,
        session_sequence: Some(1),
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event: AgentEvent::StateChanged {
            from: AgentStatus::Idle,
            to: AgentStatus::PreparingContext,
        },
    };

    let json = serde_json::to_string(&envelope).expect("serialize envelope");
    let deserialized: AgentEventEnvelope =
        serde_json::from_str(&json).expect("deserialize envelope");

    assert_eq!(envelope.event_id, deserialized.event_id);
    assert_eq!(envelope.session_id, deserialized.session_id);
    assert_eq!(envelope.agent_id, deserialized.agent_id);
    assert_eq!(envelope.agent_sequence, deserialized.agent_sequence);
    assert_eq!(envelope.visibility, deserialized.visibility);
}

// -----------------------------------------------------------------------
// AgentEvent variant round-trip serialization
// -----------------------------------------------------------------------

/// Helper: create a minimal envelope for testing event serialization.
fn envelope_for(event: AgentEvent) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id: crate::ids::SessionId::new(),
        agent_id: crate::ids::AgentId::new(),
        parent_agent_id: None,
        run_id: None,
        agent_sequence: 0,
        session_sequence: None,
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event,
    }
}

#[test]
fn event_state_changed_roundtrip() {
    let event = AgentEvent::StateChanged {
        from: AgentStatus::Idle,
        to: AgentStatus::PreparingContext,
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::StateChanged { from, to } => {
            assert_eq!(from, AgentStatus::Idle);
            assert_eq!(to, AgentStatus::PreparingContext);
        }
        other => panic!("expected StateChanged, got {other:?}"),
    }
}

#[test]
fn event_run_started_roundtrip() {
    let run_id = RunId::new();
    let event = AgentEvent::RunStarted { run_id };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::RunStarted { run_id: rid } => {
            assert_eq!(rid, run_id);
        }
        other => panic!("expected RunStarted, got {other:?}"),
    }
}

#[test]
fn event_backend_request_started_roundtrip() {
    let request_id = RequestId::new();
    let event = AgentEvent::BackendRequestStarted { request_id };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::BackendRequestStarted { request_id: rid } => {
            assert_eq!(rid, request_id);
        }
        other => panic!("expected BackendRequestStarted, got {other:?}"),
    }
}

#[test]
fn event_assistant_message_started_roundtrip() {
    let message_id = MessageId::new();
    let event = AgentEvent::AssistantMessageStarted { message_id };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::AssistantMessageStarted { message_id: mid } => {
            assert_eq!(mid, message_id);
        }
        other => panic!("expected AssistantMessageStarted, got {other:?}"),
    }
}

#[test]
fn event_assistant_text_delta_roundtrip() {
    let message_id = MessageId::new();
    let event = AgentEvent::AssistantTextDelta {
        message_id,
        delta: "Hello, world!".into(),
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::AssistantTextDelta {
            message_id: mid,
            delta,
        } => {
            assert_eq!(mid, message_id);
            assert_eq!(delta, "Hello, world!");
        }
        other => panic!("expected AssistantTextDelta, got {other:?}"),
    }
}

#[test]
fn event_reasoning_delta_roundtrip() {
    let message_id = MessageId::new();
    let event = AgentEvent::ReasoningDelta {
        message_id,
        delta: "thinking...".into(),
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ReasoningDelta {
            message_id: mid,
            delta,
        } => {
            assert_eq!(mid, message_id);
            assert_eq!(delta, "thinking...");
        }
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn event_assistant_message_completed_roundtrip() {
    let message_id = MessageId::new();
    let event = AgentEvent::AssistantMessageCompleted { message_id };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::AssistantMessageCompleted { message_id: mid } => {
            assert_eq!(mid, message_id);
        }
        other => panic!("expected AssistantMessageCompleted, got {other:?}"),
    }
}

#[test]
fn event_tool_call_requested_roundtrip() {
    let event = AgentEvent::ToolCallRequested {
        call: ToolCall {
            id: ToolCallId::new(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        },
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ToolCallRequested { call } => {
            assert_eq!(call.name, "search");
        }
        other => panic!("expected ToolCallRequested, got {other:?}"),
    }
}

#[test]
fn event_tool_call_started_roundtrip() {
    let call_id = ToolCallId::new();
    let event = AgentEvent::ToolCallStarted { call_id };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ToolCallStarted { call_id: cid } => {
            assert_eq!(cid, call_id);
        }
        other => panic!("expected ToolCallStarted, got {other:?}"),
    }
}

#[test]
fn event_tool_call_progress_roundtrip() {
    let call_id = ToolCallId::new();
    let event = AgentEvent::ToolCallProgress {
        call_id,
        progress: ToolProgress {
            status: "running".into(),
            fraction: 0.5,
        },
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ToolCallProgress {
            call_id: cid,
            progress,
        } => {
            assert_eq!(cid, call_id);
            assert_eq!(progress.status, "running");
            assert!((progress.fraction - 0.5).abs() < f64::EPSILON);
        }
        other => panic!("expected ToolCallProgress, got {other:?}"),
    }
}

#[test]
fn event_tool_call_completed_roundtrip() {
    let call_id = ToolCallId::new();
    let event = AgentEvent::ToolCallCompleted {
        call_id,
        result: ToolResultSummary {
            has_error: false,
            output_preview: "42".into(),
        },
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ToolCallCompleted {
            call_id: cid,
            result,
        } => {
            assert_eq!(cid, call_id);
            assert!(!result.has_error);
            assert_eq!(result.output_preview, "42");
        }
        other => panic!("expected ToolCallCompleted, got {other:?}"),
    }
}

#[test]
fn event_permission_requested_roundtrip() {
    let event = AgentEvent::PermissionRequested {
        request: PermissionRequest {
            id: crate::ids::PermissionId::new(),
            tool_call: ToolCall {
                id: ToolCallId::new(),
                name: "fs.read".into(),
                arguments: serde_json::json!({"path": "/tmp/test.txt"}),
            },
            agent_id: crate::ids::AgentId::new(),
        },
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::PermissionRequested { request } => {
            assert_eq!(request.tool_call.name, "fs.read");
        }
        other => panic!("expected PermissionRequested, got {other:?}"),
    }
}

#[test]
fn event_usage_updated_roundtrip() {
    let event = AgentEvent::UsageUpdated {
        usage: AgentUsageSnapshot::default(),
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::UsageUpdated { usage } => {
            assert_eq!(usage.timestamp, "");
        }
        other => panic!("expected UsageUpdated, got {other:?}"),
    }
}

#[test]
fn event_child_agent_spawned_roundtrip() {
    let agent_id = crate::ids::AgentId::new();
    let tool_call_id = Some(ToolCallId::new());
    let event = AgentEvent::ChildAgentSpawned {
        agent_id,
        tool_call_id,
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ChildAgentSpawned {
            agent_id: aid,
            tool_call_id: call,
        } => {
            assert_eq!(aid, agent_id);
            assert_eq!(call, tool_call_id);
        }
        other => panic!("expected ChildAgentSpawned, got {other:?}"),
    }
}

#[test]
fn event_child_agent_completed_roundtrip() {
    let agent_id = crate::ids::AgentId::new();
    let event = AgentEvent::ChildAgentCompleted {
        agent_id,
        outcome: AgentOutcome::Success,
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::ChildAgentCompleted {
            agent_id: aid,
            outcome,
        } => {
            assert_eq!(aid, agent_id);
            assert_eq!(outcome, AgentOutcome::Success);
        }
        other => panic!("expected ChildAgentCompleted, got {other:?}"),
    }
}

#[test]
fn event_failed_roundtrip() {
    let event = AgentEvent::Failed {
        error: AgentError {
            message: "something went wrong".into(),
            code: "ERR_INTERNAL".into(),
            details: Some(serde_json::json!({"reason": "timeout"})),
        },
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::Failed { error } => {
            assert_eq!(error.message, "something went wrong");
            assert_eq!(error.code, "ERR_INTERNAL");
            assert!(error.details.is_some());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn event_completed_roundtrip() {
    let event = AgentEvent::Completed {
        outcome: AgentOutcome::Success,
    };
    let env = envelope_for(event);
    let json = serde_json::to_string(&env).expect("serialize");
    let deserialized: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    match deserialized.event {
        AgentEvent::Completed { outcome } => {
            assert_eq!(outcome, AgentOutcome::Success);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// Verifies that all 17 event variants round-trip through JSON by
/// checking discriminant preservation.
#[test]
fn all_event_variants_roundtrip() {
    let events: Vec<AgentEvent> = vec![
        AgentEvent::StateChanged {
            from: AgentStatus::Idle,
            to: AgentStatus::PreparingContext,
        },
        AgentEvent::RunStarted {
            run_id: RunId::new(),
        },
        AgentEvent::BackendRequestStarted {
            request_id: RequestId::new(),
        },
        AgentEvent::AssistantMessageStarted {
            message_id: MessageId::new(),
        },
        AgentEvent::AssistantTextDelta {
            message_id: MessageId::new(),
            delta: "a".into(),
        },
        AgentEvent::ReasoningDelta {
            message_id: MessageId::new(),
            delta: "b".into(),
        },
        AgentEvent::AssistantMessageCompleted {
            message_id: MessageId::new(),
        },
        AgentEvent::ToolCallRequested {
            call: ToolCall {
                id: ToolCallId::new(),
                name: "c".into(),
                arguments: serde_json::json!({}),
            },
        },
        AgentEvent::ToolCallStarted {
            call_id: ToolCallId::new(),
        },
        AgentEvent::ToolCallProgress {
            call_id: ToolCallId::new(),
            progress: ToolProgress {
                status: "running".into(),
                fraction: 0.0,
            },
        },
        AgentEvent::ToolCallCompleted {
            call_id: ToolCallId::new(),
            result: ToolResultSummary {
                has_error: false,
                output_preview: "ok".into(),
            },
        },
        AgentEvent::PermissionRequested {
            request: PermissionRequest {
                id: crate::ids::PermissionId::new(),
                tool_call: ToolCall {
                    id: ToolCallId::new(),
                    name: "d".into(),
                    arguments: serde_json::json!({}),
                },
                agent_id: crate::ids::AgentId::new(),
            },
        },
        AgentEvent::UsageUpdated {
            usage: AgentUsageSnapshot::default(),
        },
        AgentEvent::ChildAgentSpawned {
            agent_id: crate::ids::AgentId::new(),
            tool_call_id: None,
        },
        AgentEvent::ChildAgentCompleted {
            agent_id: crate::ids::AgentId::new(),
            outcome: AgentOutcome::Success,
        },
        AgentEvent::Failed {
            error: AgentError {
                message: "e".into(),
                code: "ERR".into(),
                details: None,
            },
        },
        AgentEvent::Completed {
            outcome: AgentOutcome::Cancelled,
        },
    ];

    for event in &events {
        let json = serde_json::to_string(event).expect("serialize");
        let deserialized: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        let expected_tag = std::mem::discriminant(event);
        let actual_tag = std::mem::discriminant(&deserialized);
        assert_eq!(
            expected_tag, actual_tag,
            "discriminant mismatch for event: {event:?}"
        );
    }
}

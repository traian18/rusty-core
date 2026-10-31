use super::*;
use crate::backend::ExecutionEvent;
use crate::ids::{AgentId, PermissionId, RunId, ToolCallId};

/// Round-trip JSON serialization of `AgentCommand::StartRun`.
#[test]
fn start_run_roundtrip() {
    let cmd = AgentCommand::StartRun {
        input: UserInput {
            text: "Hello".into(),
            attachments: vec![Attachment {
                mime_type: "text/plain".into(),
                data: b"hello world".to_vec(),
            }],
        },
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::StartRun { input } => {
            assert_eq!(input.text, "Hello");
            assert_eq!(input.attachments.len(), 1);
            assert_eq!(input.attachments[0].mime_type, "text/plain");
        }
        other => panic!("expected StartRun, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::BackendEvent`.
#[test]
fn backend_event_roundtrip() {
    let cmd = AgentCommand::BackendEvent {
        run_id: RunId::new(),
        event: ExecutionEvent::TextDelta {
            request_id: crate::ids::RequestId::new(),
            delta: "test delta".into(),
        },
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::BackendEvent { run_id: _, event } => match event {
            ExecutionEvent::TextDelta { delta, .. } => {
                assert_eq!(delta, "test delta");
            }
            other => panic!("expected TextDelta, got {other:?}"),
        },
        other => panic!("expected BackendEvent, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::ToolCompleted`.
#[test]
fn tool_completed_roundtrip() {
    let cmd = AgentCommand::ToolCompleted {
        call_id: ToolCallId::new(),
        result: ToolResult {
            call_id: ToolCallId::new(),
            output: serde_json::json!({"key": "value"}),
            is_error: false,
        },
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::ToolCompleted { result, .. } => {
            assert!(!result.is_error);
        }
        other => panic!("expected ToolCompleted, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::ToolFailed`.
#[test]
fn tool_failed_roundtrip() {
    let cmd = AgentCommand::ToolFailed {
        call_id: ToolCallId::new(),
        error: ToolError::Timeout,
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::ToolFailed { error, .. } => {
            assert!(matches!(error, ToolError::Timeout));
        }
        other => panic!("expected ToolFailed, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::PermissionResolved`.
#[test]
fn permission_resolved_roundtrip() {
    let cmd = AgentCommand::PermissionResolved {
        id: PermissionId::new(),
        decision: PermissionDecision::Approved,
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::PermissionResolved { decision, .. } => {
            assert!(matches!(decision, PermissionDecision::Approved));
        }
        other => panic!("expected PermissionResolved, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::ChildCompleted`.
#[test]
fn child_completed_roundtrip() {
    let cmd = AgentCommand::ChildCompleted {
        agent_id: AgentId::new(),
        result: AgentResult {
            summary: "done".into(),
            usage: AgentUsageSummary::default(),
            gate_passed: None,
        },
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::ChildCompleted { result, .. } => {
            assert_eq!(result.summary, "done");
        }
        other => panic!("expected ChildCompleted, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentCommand::ChildFailed`.
#[test]
fn child_failed_roundtrip() {
    let cmd = AgentCommand::ChildFailed {
        agent_id: AgentId::new(),
        error: AgentError {
            message: "something went wrong".into(),
            code: "ERR_INTERNAL".into(),
            details: Some(serde_json::json!({"reason": "timeout"})),
        },
    };

    let json = serde_json::to_string(&cmd).expect("serialize");
    let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");

    match deserialized {
        AgentCommand::ChildFailed { error, .. } => {
            assert_eq!(error.message, "something went wrong");
            assert_eq!(error.code, "ERR_INTERNAL");
            assert!(error.details.is_some());
        }
        other => panic!("expected ChildFailed, got {other:?}"),
    }
}

/// Round-trip JSON serialization of unit variants.
#[test]
fn unit_variants_roundtrip() {
    for cmd in [
        AgentCommand::Cancel,
        AgentCommand::Pause,
        AgentCommand::Resume,
        AgentCommand::StartNextQueuedRun,
    ] {
        let json = serde_json::to_string(&cmd).expect("serialize");
        let deserialized: AgentCommand = serde_json::from_str(&json).expect("deserialize");
        let expected_tag = std::mem::discriminant(&cmd);
        let actual_tag = std::mem::discriminant(&deserialized);
        assert_eq!(
            expected_tag, actual_tag,
            "discriminant mismatch for {cmd:?}"
        );
    }
}

/// Round-trip JSON serialization of `PermissionDecision::Denied`.
#[test]
fn permission_decision_denied_roundtrip() {
    let decision = PermissionDecision::Denied;
    let json = serde_json::to_string(&decision).expect("serialize");
    let deserialized: PermissionDecision = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deserialized, PermissionDecision::Denied));
}

/// Round-trip JSON serialization of `AgentStatus`.
#[test]
fn agent_status_roundtrip() {
    let statuses = [
        AgentStatus::Idle,
        AgentStatus::PreparingContext,
        AgentStatus::WaitingForBackend,
        AgentStatus::Streaming,
        AgentStatus::Executing,
        AgentStatus::WaitingForPermission,
        AgentStatus::WaitingForChildren,
        AgentStatus::Paused,
        AgentStatus::Completed,
        AgentStatus::Cancelled,
        AgentStatus::Failed,
    ];

    for status in &statuses {
        let json = serde_json::to_string(status).expect("serialize");
        let deserialized: AgentStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*status, deserialized);
    }
}

/// Round-trip JSON serialization of `AgentOperation`.
#[test]
fn agent_operation_backend_request_roundtrip() {
    let op = AgentOperation::BackendRequest {
        request_id: crate::ids::RequestId::new(),
    };
    let json = serde_json::to_string(&op).expect("serialize");
    let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
    match deserialized {
        AgentOperation::BackendRequest { .. } => {}
        other => panic!("expected BackendRequest, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentOperation::Tools`.
#[test]
fn agent_operation_tools_roundtrip() {
    let op = AgentOperation::Tools {
        calls: vec![ToolCallId::new(), ToolCallId::new()],
    };
    let json = serde_json::to_string(&op).expect("serialize");
    let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
    match deserialized {
        AgentOperation::Tools { calls } => {
            assert_eq!(calls.len(), 2);
        }
        other => panic!("expected Tools, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentOperation::Children`.
#[test]
fn agent_operation_children_roundtrip() {
    let op = AgentOperation::Children {
        agents: vec![AgentId::new()],
    };
    let json = serde_json::to_string(&op).expect("serialize");
    let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
    match deserialized {
        AgentOperation::Children { agents } => {
            assert_eq!(agents.len(), 1);
        }
        other => panic!("expected Children, got {other:?}"),
    }
}

/// Round-trip JSON serialization of `AgentOperation::Permission`.
#[test]
fn agent_operation_permission_roundtrip() {
    let op = AgentOperation::Permission {
        request_id: PermissionId::new(),
    };
    let json = serde_json::to_string(&op).expect("serialize");
    let deserialized: AgentOperation = serde_json::from_str(&json).expect("deserialize");
    match deserialized {
        AgentOperation::Permission { .. } => {}
        other => panic!("expected Permission, got {other:?}"),
    }
}

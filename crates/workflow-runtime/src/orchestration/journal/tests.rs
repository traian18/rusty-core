use super::actions::evidence_locations;
use super::*;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::{
    ids::ToolCallId,
    tools::{ToolCall, ToolResultSummary},
};
use rusty_workflow_core::{AttemptStatus, CommandRun, StepAttempt, StepRun};
use serde_json::json;
use serde_json::Value;

fn call(recorder: &mut ActionRecorder, name: &str, arguments: Value, failed: bool, output: &str) {
    let id = ToolCallId::new();
    recorder.observe(&AgentEvent::ToolCallRequested {
        call: ToolCall {
            id,
            name: name.into(),
            arguments,
        },
    });
    recorder.observe(&AgentEvent::ToolCallCompleted {
        call_id: id,
        result: ToolResultSummary {
            has_error: failed,
            output_preview: output.into(),
        },
    });
}

#[test]
fn records_what_the_tool_calls_did() {
    let mut recorder = ActionRecorder::default();
    call(
        &mut recorder,
        "read_file",
        json!({"path": "src/a.rs"}),
        false,
        "",
    );
    call(
        &mut recorder,
        "edit_file",
        json!({"path": "src/a.rs"}),
        false,
        "",
    );
    call(
        &mut recorder,
        "write_file",
        json!({"path": "src/b.rs"}),
        true,
        "refused",
    );
    call(
        &mut recorder,
        "write_file",
        json!({"path": "src/a.rs"}),
        false,
        "",
    );
    call(
        &mut recorder,
        "run_command",
        json!({"command": "cargo test"}),
        false,
        "…\nExit code: 101",
    );
    call(
        &mut recorder,
        "run_command",
        json!({"command": ["ls", "-la"]}),
        false,
        "exit code: 0",
    );
    call(
        &mut recorder,
        "run_check",
        json!({"check": "typecheck"}),
        true,
        "",
    );
    call(
        &mut recorder,
        "request_research",
        json!({"question": "How is X configured?"}),
        false,
        "",
    );
    let actions = recorder.finish();
    assert_eq!(actions.files_changed, vec!["src/a.rs"]);
    assert_eq!(actions.files_read, vec!["src/a.rs"]);
    assert_eq!(
        actions.commands,
        vec![
            CommandRun {
                command: "cargo test".into(),
                failed: true
            },
            CommandRun {
                command: "ls -la".into(),
                failed: false
            },
            CommandRun {
                command: "check: typecheck".into(),
                failed: true
            },
        ]
    );
    assert_eq!(actions.research, vec!["How is X configured?"]);
    assert_eq!(actions.tool_errors, 2);
    let rendered = render_actions(&actions);
    assert!(rendered.contains("- Changed: src/a.rs"));
    assert!(rendered.contains("`cargo test` (failed)"));
}

#[test]
fn research_evidence_becomes_locations_the_later_steps_see() {
    let mut recorder = ActionRecorder::default();
    let answer = "## Answer\nIn the policy.\n## Evidence\n- rusty-core/crates/rusty-context/src/policy.rs:128-150 computes the reading\n- https://docs.rs/serde explains it\n- see the design\n## Unknowns\nNone";
    call(
        &mut recorder,
        "request_research",
        json!({"question": "Where is pressure computed?"}),
        false,
        answer,
    );
    let actions = recorder.finish();
    assert_eq!(
        actions.locations,
        vec!["rusty-core/crates/rusty-context/src/policy.rs:128-150 computes the reading"]
    );
    assert!(render_actions(&actions)
        .contains("- Research found (read these lines rather than searching again): rusty-core/crates/rusty-context/src/policy.rs:128-150"));
}

#[test]
fn evidence_is_read_from_the_research_reply_as_it_really_arrives() {
    let findings = "## Answer\nIt is cumulative.\n\n## Evidence\n\n- **Badge:** `rusty-ide/src/features/agent-chat/ui/AgentTabHeader.tsx:119` renders `runUsage`; `rusty-ide/src/lib/ui/TokenBadge/TokenBadge.tsx:21-46` displays totals.\n- **Budget:** `rusty-core/crates/rusty-context/src/policy.rs:55-65,82-123` applies a 90% window.\n- The SDK docs at https://example.com/a.ts:3 say so.\n\n## Unknowns\nNone";
    let reply = json!({ "findings": findings }).to_string();
    let found = evidence_locations(&reply);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0]
        .starts_with("Badge: `rusty-ide/src/features/agent-chat/ui/AgentTabHeader.tsx:119`"));
    assert!(found[1].contains("policy.rs:55-65,82-123"));
}

#[test]
fn a_retried_step_sees_what_its_last_attempt_did_and_handed_on() {
    let mut step = StepRun::default();
    assert!(previous_attempt(&step).is_none());
    let mut details = rusty_workflow_core::AttemptDetails::default();
    details.actions.files_changed.push("src/lib.rs".into());
    step.attempts.push(StepAttempt {
        attempt: 1,
        status: AttemptStatus::Succeeded,
        output: Some(json!("Implemented the parser fix.")),
        error: None,
        details,
    });
    step.attempts.push(StepAttempt {
        attempt: 2,
        status: AttemptStatus::Running,
        output: None,
        error: None,
        details: Default::default(),
    });
    let text = previous_attempt(&step).unwrap();
    assert!(text.starts_with("Attempt 1 finished"));
    assert!(text.contains("- Changed: src/lib.rs"));
    assert!(text.contains("Implemented the parser fix."));
}

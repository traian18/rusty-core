use super::*;

use harness_protocol::tools::{ToolCall, ToolResultSummary};

fn tool_pair(name: &str, output: &str) -> [AgentMessage; 2] {
    let id = ToolCallId::new();
    [
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                call: ToolCall {
                    id,
                    name: name.to_string(),
                    arguments: serde_json::Value::Null,
                },
            }],
            created_at: Timestamp::now(),
        },
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                call_id: id,
                result: ToolResultSummary {
                    has_error: false,
                    output_preview: output.to_string(),
                },
            }],
            created_at: Timestamp::now(),
        },
    ]
}

fn output_of(message: &AgentMessage) -> &str {
    match &message.content[0] {
        ContentBlock::ToolResult { result, .. } => &result.output_preview,
        _ => panic!("expected a tool result"),
    }
}

const STAMP: &str = r#"[rusty:condensed git-log · 18,294->4,960 chars · full output: job_output id="out-3f9c2a81d4e70b5a" (add pattern, or start_line and end_line, to read part)]"#;

#[test]
fn elision_keeps_the_recovery_stamp_of_condensed_output() {
    let mut messages = tool_pair(
        "run_command",
        &format!("{}\n{STAMP}", "a297ac4 subject\n".repeat(40)),
    );
    assert_eq!(
        elide_tool_results(&mut messages, 0, &Retention::default()),
        1
    );
    let stub = output_of(&messages[1]);
    assert!(stub.starts_with("[run_command result elided to save context"));
    assert!(stub.ends_with(&format!("…\n{STAMP}")));
}

#[test]
fn a_stub_is_never_elided_again() {
    let mut messages = tool_pair("run_command", &format!("{}\n{STAMP}", "x".repeat(2_000)));
    elide_tool_results(&mut messages, 0, &Retention::default());
    let once = output_of(&messages[1]).to_string();
    assert!(
        once.len() > ELIDE_MIN_CHARS,
        "the stamp makes the stub long enough to be picked again"
    );
    assert_eq!(
        elide_tool_results(&mut messages, 0, &Retention::default()),
        0
    );
    assert_eq!(output_of(&messages[1]), once);
}

#[test]
fn output_without_a_stamp_is_elided_as_before() {
    let mut messages = tool_pair("read_file", &"y".repeat(1_000));
    elide_tool_results(&mut messages, 0, &Retention::default());
    let stub = output_of(&messages[1]);
    assert!(stub.ends_with('…'));
    assert!(!stub.contains(STAMP_PREFIX));
}

#[test]
fn only_a_whole_stamp_line_counts() {
    assert_eq!(recovery_stamp(&format!("log\n{STAMP}\n")), Some(STAMP));
    assert_eq!(recovery_stamp("see [rusty:condensed x] inline"), None);
    assert_eq!(
        recovery_stamp(&format!("[rusty:{}]", "z".repeat(MAX_STAMP_CHARS))),
        None
    );
}

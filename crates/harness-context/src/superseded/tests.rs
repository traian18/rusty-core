use super::*;
use harness_protocol::ids::{MessageId, Timestamp};
use harness_protocol::messages::MessageRole;
use harness_protocol::tools::{ToolCall, ToolResultSummary};
use serde_json::json;

/// A tool call and its result, as two messages.
fn call(name: &str, arguments: Value, output: &str) -> Vec<AgentMessage> {
    let id = ToolCallId::new();
    let message = |role, block| AgentMessage {
        id: MessageId::new(),
        role,
        content: vec![block],
        created_at: Timestamp::now(),
    };
    vec![
        message(
            MessageRole::Assistant,
            ContentBlock::ToolUse {
                call: ToolCall {
                    id,
                    name: name.into(),
                    arguments,
                },
            },
        ),
        message(
            MessageRole::Tool,
            ContentBlock::ToolResult {
                call_id: id,
                result: ToolResultSummary {
                    has_error: false,
                    output_preview: output.into(),
                },
            },
        ),
    ]
}

fn outputs(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { result, .. } => Some(result.output_preview.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_file_read_again_keeps_only_its_newest_copy() {
    let mut history = [
        call("read_file", json!({"path": "src/a.ts"}), "old a"),
        call("read_file", json!({"path": "src/b.ts"}), "b"),
        call(
            "read_file",
            json!({"path": "./src/a.ts", "start_line": 1, "end_line": 20}),
            "a lines 1-20",
        ),
        call("edit_file", json!({"path": "src/a.ts"}), "edited"),
        call("read_file", json!({"path": "src/a.ts"}), "new a"),
    ]
    .concat();
    let len = history.len();
    assert_eq!(stub_superseded_reads(&mut history, len), 2);
    let out = outputs(&history);
    assert!(
        out[0].starts_with("[older read replaced: src/a.ts"),
        "{out:?}"
    );
    assert_eq!(out[1], "b");
    assert!(
        out[2].starts_with("[older read replaced:"),
        "a range is covered by a later whole read"
    );
    assert_eq!(out[3], "edited");
    assert_eq!(out[4], "new a");
}

#[test]
fn a_different_range_or_request_does_not_supersede() {
    let mut history = [
        call(
            "read_file",
            json!({"path": "a.ts", "start_line": 1, "end_line": 50}),
            "1-50",
        ),
        call(
            "read_file",
            json!({"path": "a.ts", "start_line": 51, "end_line": 99}),
            "51-99",
        ),
        call(
            "read_file",
            json!({"path": "a.ts", "request": "the reducer"}),
            "excerpt",
        ),
        call(
            "read_file",
            json!({"path": "a.ts", "request": "the reducer"}),
            "excerpt again",
        ),
    ]
    .concat();
    let len = history.len();
    assert_eq!(stub_superseded_reads(&mut history, len), 1);
    let out = outputs(&history);
    assert_eq!(&out[..2], ["1-50", "51-99"]);
    assert!(out[2].starts_with("[older read replaced:"));
    assert_eq!(out[3], "excerpt again");
}

#[test]
fn only_the_history_before_the_rewrite_point_changes() {
    let mut history = [
        call("read_file", json!({"path": "a.ts"}), "first"),
        call("read_file", json!({"path": "a.ts"}), "second"),
    ]
    .concat();
    // The second read lies past the point: nothing before it is superseded yet.
    assert_eq!(stub_superseded_reads(&mut history, 2), 0);
    assert_eq!(outputs(&history), ["first", "second"]);
}

#[test]
fn the_rewrite_point_moves_in_whole_steps() {
    let big = "x".repeat(400);
    let history: Vec<AgentMessage> = (0..10)
        .flat_map(|_| call("read_file", json!({"path": "a.ts"}), &big))
        .collect();
    // 4240 characters in all (424 per pair): steps of 1100 end at 3300, in the 8th pair.
    let at_1000 = rewrite_point(&history, 1_100);
    assert_eq!(at_1000, 16);
    assert_eq!(rewrite_point(&history, 100_000), 0, "shorter than one step");
    // Adding a little more history does not move the point until a boundary is crossed.
    let mut longer = history.clone();
    longer.extend(call("list_files", json!({}), "x"));
    assert_eq!(rewrite_point(&longer, 1_100), at_1000);
}

#[test]
fn replacing_twice_changes_nothing_more() {
    let mut history = [
        call("read_file", json!({"path": "a.ts"}), "first"),
        call("read_file", json!({"path": "a.ts"}), "second"),
    ]
    .concat();
    assert_eq!(stub_superseded_reads(&mut history, 4), 1);
    let once = outputs(&history);
    assert_eq!(stub_superseded_reads(&mut history, 4), 0);
    assert_eq!(outputs(&history), once);
}

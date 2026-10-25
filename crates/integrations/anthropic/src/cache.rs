//! Prompt caching for the Messages API.
//!
//! Anthropic caches a prompt only up to the points a request marks with
//! `cache_control`, at most four per request. Every request gets three:
//! - the system prompt and the tool list, which never change during a run;
//! - the last block of the last message, which moves forward with each call,
//!   so the next call reads everything before it from the cache (a lookup
//!   also checks the 20 blocks before a mark, which covers the previous one).
//!
//! A prompt shorter than the model's minimum is not cached and the marks are
//! ignored, so they are always safe to send. The GitHub Copilot integration
//! uses this client for Claude models and gets the same marks.

use serde_json::{json, Value};

/// Blocks that cannot carry a cache mark.
const UNMARKABLE: &[&str] = &["thinking", "redacted_thinking"];

/// Adds the cache marks to a serialized Messages request.
pub fn add_cache_breakpoints(body: &mut Value) {
    let ephemeral = json!({ "type": "ephemeral" });
    if let Some(system) = body.get_mut("system") {
        if let Some(text) = system.as_str().map(str::to_owned) {
            *system = json!([{ "type": "text", "text": text, "cache_control": ephemeral }]);
        } else if let Some(last) = system.as_array_mut().and_then(|blocks| blocks.last_mut()) {
            mark(last, &ephemeral);
        }
    }
    if let Some(last) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
    {
        mark(last, &ephemeral);
    }
    if let Some(block) = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|messages| messages.last_mut())
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut)
        .and_then(|blocks| {
            blocks
                .iter_mut()
                .rev()
                .find(|block| !UNMARKABLE.contains(&block["type"].as_str().unwrap_or("")))
        })
    {
        mark(block, &ephemeral);
    }
}

fn mark(target: &mut Value, ephemeral: &Value) {
    if let Some(fields) = target.as_object_mut() {
        fields
            .entry("cache_control")
            .or_insert_with(|| ephemeral.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_system_prompt_the_tools_and_the_last_message() {
        let mut body = json!({
            "system": "You are Build.",
            "tools": [{"name": "read_file"}, {"name": "edit_file"}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "task"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "read_file", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "file"}]}
            ]
        });
        add_cache_breakpoints(&mut body);
        assert_eq!(body["system"][0]["text"], "You are Build.");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert!(body["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let marks = body.to_string().matches("cache_control").count();
        assert!(
            marks <= 4,
            "Anthropic allows at most four marks, got {marks}"
        );
    }

    #[test]
    fn skips_thinking_blocks_and_works_without_system_or_tools() {
        let mut body = json!({
            "messages": [{"role": "assistant", "content": [
                {"type": "text", "text": "answer"},
                {"type": "thinking", "thinking": "..."}
            ]}]
        });
        add_cache_breakpoints(&mut body);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(body["messages"][0]["content"][1]
            .get("cache_control")
            .is_none());
        assert!(body.get("system").is_none());
    }

    #[test]
    fn a_caller_mark_is_kept() {
        let mut body = json!({"system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral", "ttl": "1h"}}], "messages": []});
        add_cache_breakpoints(&mut body);
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
    }
}

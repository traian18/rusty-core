//! Parsing helpers for the Codex CLI's `--json` (JSONL) output. Pure
//! functions, kept separate from the subprocess-driving code in
//! `backend.rs` so they're testable against recorded JSON without spawning
//! a process.
//!
//! # Schema (verified against a real `codex exec --json ...` invocation,
//! CLI version 0.146.0 — a genuinely different shape from Claude Code's, as
//! expected; see `crates/integrations/codex/PLAN.md`)
//!
//! - `{"type":"thread.started","thread_id":"<uuid>"}` — first line;
//!   `thread_id` is threaded into `codex exec resume <thread_id>` on the
//!   *next* call, mirroring Claude Code's `--resume`.
//! - `{"type":"turn.started"}` — ignored.
//! - `{"type":"item.completed","item":{"id":"...","type":"agent_message","text":"..."}}` —
//!   **unlike Claude Code**, each line is a *complete* item, not an
//!   ever-growing accumulation — so `backend.rs` forwards `text` directly as
//!   one delta per completed item rather than diffing against previous
//!   lines. Other observed/plausible `item.type` values (`reasoning`,
//!   `command_execution`, `file_change`, ...) are intentionally ignored for
//!   now — only `agent_message` is verified and handled.
//! - `{"type":"turn.completed","usage":{"input_tokens":...,"cached_input_tokens":...,"cache_write_input_tokens":...,"output_tokens":...,"reasoning_output_tokens":...}}` —
//!   terminal line. No cost field is reported by the CLI at all (ChatGPT-plan
//!   usage doesn't map to a per-token USD figure the way an API key does),
//!   so this backend always reports `Cost::default()` (unknown).
//!
//! A `turn.failed`/error line was **not** observed in this exploration (only
//! success paths were exercised) — `extract_error` below is a defensive,
//! best-effort catch-all (`type` containing `"error"` or `"failed"`) rather
//! than a verified schema, since triggering a real failure wasn't done.

use harness_protocol::usage::{ModelUsage, UsageValue};

pub fn extract_thread_id(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("thread.started") {
        return None;
    }
    value.get("thread_id").and_then(|t| t.as_str()).map(str::to_string)
}

/// Returns the text of a completed `agent_message` item, if `value` is one.
pub fn extract_agent_message_text(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("item.completed") {
        return None;
    }
    let item = value.get("item")?;
    if item.get("type").and_then(|t| t.as_str()) != Some("agent_message") {
        return None;
    }
    item.get("text").and_then(|t| t.as_str()).map(str::to_string)
}

pub struct TurnCompleted {
    pub usage: ModelUsage,
}

pub fn extract_turn_completed(value: &serde_json::Value) -> Option<TurnCompleted> {
    if value.get("type").and_then(|t| t.as_str()) != Some("turn.completed") {
        return None;
    }
    let usage_value = value.get("usage");
    let input_tokens = usage_value.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64());
    let output_tokens = usage_value.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64());
    let cache_read_tokens = usage_value
        .and_then(|u| u.get("cached_input_tokens"))
        .and_then(|v| v.as_u64());
    let cache_write_tokens = usage_value
        .and_then(|u| u.get("cache_write_input_tokens"))
        .and_then(|v| v.as_u64());
    let reasoning_tokens = usage_value
        .and_then(|u| u.get("reasoning_output_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        _ => None,
    };

    Some(TurnCompleted {
        usage: ModelUsage {
            input_tokens: UsageValue::new(input_tokens),
            output_tokens: UsageValue::new(output_tokens),
            cache_read_tokens: UsageValue::new(cache_read_tokens),
            cache_write_tokens: UsageValue::new(cache_write_tokens),
            reasoning_tokens: UsageValue::new(reasoning_tokens),
            total_tokens: UsageValue::new(total_tokens),
        },
    })
}

/// Best-effort, **unverified** error detection — see module docs. Returns a
/// message if `value`'s `type` field contains `"error"` or `"failed"`.
pub fn extract_error(value: &serde_json::Value) -> Option<String> {
    let kind = value.get("type").and_then(|t| t.as_str())?;
    if kind.contains("error") || kind.contains("failed") {
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| value.get("error").and_then(|e| e.as_str()))
            .unwrap_or(kind);
        Some(message.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_thread_id_from_thread_started() {
        let value = serde_json::json!({ "type": "thread.started", "thread_id": "019fc8bb-b347-7550-87fa-e57e0c0f52df" });
        assert_eq!(
            extract_thread_id(&value),
            Some("019fc8bb-b347-7550-87fa-e57e0c0f52df".to_string())
        );
    }

    #[test]
    fn ignores_turn_started() {
        let value = serde_json::json!({ "type": "turn.started" });
        assert_eq!(extract_thread_id(&value), None);
        assert_eq!(extract_agent_message_text(&value), None);
        assert!(extract_turn_completed(&value).is_none());
    }

    #[test]
    fn extracts_agent_message_text() {
        let value = serde_json::json!({
            "type": "item.completed",
            "item": { "id": "item_0", "type": "agent_message", "text": "pong" }
        });
        assert_eq!(extract_agent_message_text(&value), Some("pong".to_string()));
    }

    #[test]
    fn ignores_non_agent_message_items() {
        let value = serde_json::json!({
            "type": "item.completed",
            "item": { "id": "item_1", "type": "reasoning", "text": "thinking..." }
        });
        assert_eq!(extract_agent_message_text(&value), None);
    }

    #[test]
    fn extracts_turn_completed_usage() {
        let value = serde_json::json!({
            "type": "turn.completed",
            "usage": {
                "input_tokens": 14976,
                "cached_input_tokens": 11008,
                "cache_write_input_tokens": 0,
                "output_tokens": 5,
                "reasoning_output_tokens": 0
            }
        });
        let turn = extract_turn_completed(&value).expect("a turn.completed line");
        assert_eq!(turn.usage.input_tokens.value(), Some(14976));
        assert_eq!(turn.usage.output_tokens.value(), Some(5));
        assert_eq!(turn.usage.cache_read_tokens.value(), Some(11008));
        assert_eq!(turn.usage.total_tokens.value(), Some(14981));
    }

    #[test]
    fn detects_an_error_type_line() {
        let value = serde_json::json!({ "type": "turn.failed", "message": "boom" });
        assert_eq!(extract_error(&value), Some("boom".to_string()));
    }

    #[test]
    fn non_error_lines_report_no_error() {
        let value = serde_json::json!({ "type": "turn.completed" });
        assert_eq!(extract_error(&value), None);
    }
}

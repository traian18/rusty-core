//! Parsing helpers for the Claude Code CLI's `--output-format stream-json`
//! lines. Pure functions, kept separate from the subprocess-driving code in
//! `backend.rs` so they're testable against recorded JSON without spawning
//! a process.
//!
//! # Schema (verified against a real `claude -p ... --output-format
//! stream-json --verbose` invocation, CLI version 2.1.220)
//!
//! - `{"type":"system","subtype":"init","session_id":"<uuid>",...}` — first
//!   line; `session_id` is threaded into `--resume` on the *next* call so
//!   the CLI's own on-disk session state supplies conversation history
//!   (this backend never resends the full transcript — see `backend.rs`).
//! - `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]},...}` —
//!   **not incremental**: each line carries the *full* accumulated text for
//!   the in-progress message, not a delta. `backend.rs` diffs against what
//!   was already sent to produce real `TextDelta` events.
//! - `{"type":"result","subtype":"success","total_cost_usd":...,"usage":{...},"result":"..."}` —
//!   terminal line with final cost/usage/text.

use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_protocol::usage::{ModelUsage, UsageValue};

/// Finds the most recent `User`-role message and concatenates its `Text`
/// blocks — the one new turn to send to `claude -p`, since the CLI's own
/// `--resume`d session already holds every earlier turn.
pub fn extract_latest_user_text(messages: &[AgentMessage]) -> Option<String> {
    let message = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)?;
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

/// Extracts the `session_id` from a `{"type":"system","subtype":"init",...}` line.
pub fn extract_session_id(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("system") {
        return None;
    }
    if value.get("subtype").and_then(|s| s.as_str()) != Some("init") {
        return None;
    }
    value
        .get("session_id")
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

/// Extracts the full accumulated assistant text from a
/// `{"type":"assistant",...}` line, if it carries a `text` content block.
pub fn extract_assistant_text(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let content = value.get("message")?.get("content")?.as_array()?;
    let text: String = content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// The parsed content of a terminal `{"type":"result",...}` line.
pub struct ResultLine {
    pub finish_reason: String,
    pub usage: ModelUsage,
    pub cost_usd: Option<f64>,
}

/// Parses a `{"type":"result",...}` line, or `None` if `value` isn't one.
pub fn extract_result(value: &serde_json::Value) -> Option<ResultLine> {
    if value.get("type").and_then(|t| t.as_str()) != Some("result") {
        return None;
    }
    let finish_reason = value
        .get("subtype")
        .and_then(|s| s.as_str())
        .unwrap_or("end_turn")
        .to_string();
    let cost_usd = value.get("total_cost_usd").and_then(|c| c.as_f64());
    let usage_value = value.get("usage");
    let input_tokens = usage_value
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64());
    let output_tokens = usage_value
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64());
    let cache_read_tokens = usage_value
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_u64());
    let cache_write_tokens = usage_value
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        _ => None,
    };

    Some(ResultLine {
        finish_reason,
        usage: ModelUsage {
            input_tokens: UsageValue::new(input_tokens),
            output_tokens: UsageValue::new(output_tokens),
            cache_read_tokens: UsageValue::new(cache_read_tokens),
            cache_write_tokens: UsageValue::new(cache_write_tokens),
            reasoning_tokens: UsageValue::new(None),
            total_tokens: UsageValue::new(total_tokens),
        },
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::ids::{MessageId, Timestamp};

    fn user_message(text: &str) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            created_at: Timestamp::now(),
        }
    }

    fn assistant_message(text: &str) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn extracts_the_most_recent_user_message() {
        let messages = vec![
            user_message("first"),
            assistant_message("reply"),
            user_message("second"),
        ];
        assert_eq!(
            extract_latest_user_text(&messages),
            Some("second".to_string())
        );
    }

    #[test]
    fn returns_none_with_no_user_message() {
        let messages = vec![assistant_message("reply")];
        assert_eq!(extract_latest_user_text(&messages), None);
    }

    #[test]
    fn extracts_session_id_from_init_line() {
        let value = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "300a4df8-bc57-41dc-8254-76bc3dac0b7d"
        });
        assert_eq!(
            extract_session_id(&value),
            Some("300a4df8-bc57-41dc-8254-76bc3dac0b7d".to_string())
        );
    }

    #[test]
    fn ignores_non_init_system_lines() {
        let value = serde_json::json!({ "type": "system", "subtype": "thinking_tokens" });
        assert_eq!(extract_session_id(&value), None);
    }

    #[test]
    fn extracts_full_accumulated_text_from_assistant_line() {
        let value = serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "pong" }] }
        });
        assert_eq!(extract_assistant_text(&value), Some("pong".to_string()));
    }

    #[test]
    fn extracts_result_line_fields() {
        let value = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "total_cost_usd": 0.0149959,
            "usage": { "input_tokens": 10, "output_tokens": 44, "cache_read_input_tokens": 11609, "cache_creation_input_tokens": 6508 },
            "result": "pong"
        });
        let result = extract_result(&value).expect("a result line");
        assert_eq!(result.finish_reason, "success");
        assert_eq!(result.cost_usd, Some(0.0149959));
        assert_eq!(result.usage.input_tokens.value(), Some(10));
        assert_eq!(result.usage.output_tokens.value(), Some(44));
        assert_eq!(result.usage.total_tokens.value(), Some(54));
    }
}

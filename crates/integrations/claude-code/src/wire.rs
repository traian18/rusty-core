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

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Extracts tool use calls from an assistant message line.
pub fn extract_tool_uses(value: &serde_json::Value) -> Vec<ParsedToolUse> {
    if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    let Some(content) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|block| {
            let id = block.get("id")?.as_str()?.to_string();
            let name = block.get("name")?.as_str()?.to_string();
            let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
            Some(ParsedToolUse { id, name, input })
        })
        .collect()
}

/// Extracts tool results from a user message line.
pub fn extract_tool_results(value: &serde_json::Value) -> Vec<ParsedToolResult> {
    if value.get("type").and_then(|t| t.as_str()) != Some("user") {
        return Vec::new();
    }
    let Some(content) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .filter_map(|block| {
            let tool_use_id = block.get("tool_use_id")?.as_str()?.to_string();
            let is_error = block.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
            let content_val = block.get("content");
            let content_str = match content_val {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                None => String::new(),
            };
            Some(ParsedToolResult {
                tool_use_id,
                content: content_str,
                is_error,
            })
        })
        .collect()
}

/// Extracts thinking/reasoning text from an assistant message line.
pub fn extract_thinking(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let content = value.get("message")?.get("content")?.as_array()?;
    let thinking: String = content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        .filter_map(|block| block.get("thinking").and_then(|t| t.as_str()))
        .collect();
    (!thinking.is_empty()).then_some(thinking)
}

/// Extracts intermediate usage from an assistant message line.
pub fn extract_intermediate_usage(value: &serde_json::Value) -> Option<ModelUsage> {
    if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let usage_value = value.get("message")?.get("usage")?;
    let input_tokens = usage_value.get("input_tokens").and_then(|v| v.as_u64());
    let output_tokens = usage_value.get("output_tokens").and_then(|v| v.as_u64());
    let cache_read_tokens = usage_value.get("cache_read_input_tokens").and_then(|v| v.as_u64());
    let cache_write_tokens = usage_value.get("cache_creation_input_tokens").and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        _ => None,
    };
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    Some(ModelUsage {
        input_tokens: UsageValue::new(input_tokens),
        output_tokens: UsageValue::new(output_tokens),
        cache_read_tokens: UsageValue::new(cache_read_tokens),
        cache_write_tokens: UsageValue::new(cache_write_tokens),
        reasoning_tokens: UsageValue::new(None),
        total_tokens: UsageValue::new(total_tokens),
    })
}

/// The parsed content of a terminal `{"type":"result",...}` line.
pub struct ResultLine {
    pub finish_reason: String,
    pub is_error: bool,
    pub error_message: Option<String>,
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
    let is_error = value
        .get("is_error")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false);
    let error_message = is_error
        .then(|| {
            value
                .get("result")
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
        .flatten();
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
        is_error,
        error_message,
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
        assert!(!result.is_error);
        assert_eq!(result.cost_usd, Some(0.0149959));
        assert_eq!(result.usage.input_tokens.value(), Some(10));
        assert_eq!(result.usage.output_tokens.value(), Some(44));
        assert_eq!(result.usage.total_tokens.value(), Some(54));
    }

    #[test]
    fn preserves_error_shaped_success_results_from_the_cli() {
        let value = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "Not logged in · Please run /login",
            "terminal_reason": "api_error",
            "usage": {}
        });
        let result = extract_result(&value).expect("a result line");
        assert!(result.is_error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("Not logged in · Please run /login")
        );
    }

    #[test]
    fn extracts_tool_uses_from_assistant_message() {
        let value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_01LQH5yRHZkHS9EcF96EP5rY",
                        "name": "Bash",
                        "input": { "command": "echo 'hello'" }
                    }
                ]
            }
        });
        let calls = extract_tool_uses(&value);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_01LQH5yRHZkHS9EcF96EP5rY");
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "echo 'hello'");
    }

    #[test]
    fn extracts_tool_results_from_user_message() {
        let value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_01LQH5yRHZkHS9EcF96EP5rY",
                        "content": "hello",
                        "is_error": false
                    }
                ]
            }
        });
        let results = extract_tool_results(&value);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_use_id, "toolu_01LQH5yRHZkHS9EcF96EP5rY");
        assert_eq!(results[0].content, "hello");
        assert!(!results[0].is_error);
    }

    #[test]
    fn extracts_thinking_from_assistant_message() {
        let value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "thinking", "thinking": "Let me calculate..." }
                ]
            }
        });
        assert_eq!(extract_thinking(&value), Some("Let me calculate...".to_string()));
    }

    #[test]
    fn extracts_intermediate_usage_from_assistant_message() {
        let value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{ "type": "text", "text": "ok" }],
                "usage": { "input_tokens": 15, "output_tokens": 8 }
            }
        });
        let usage = extract_intermediate_usage(&value).expect("usage present");
        assert_eq!(usage.input_tokens.value(), Some(15));
        assert_eq!(usage.output_tokens.value(), Some(8));
        assert_eq!(usage.total_tokens.value(), Some(23));
    }
}

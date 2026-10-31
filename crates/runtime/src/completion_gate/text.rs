//! Trimming transcripts and text to what an evaluator may read and pass on.

use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use super::MAX_TRANSCRIPT_ENTRY_CHARS;

pub(super) fn tail(messages: &[AgentMessage], count: usize) -> &[AgentMessage] {
    &messages[messages.len().saturating_sub(count)..]
}

pub(super) fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}… (truncated)", &text[..cut]),
        None => text.to_string(),
    }
}

pub(super) fn render_transcript(messages: &[AgentMessage]) -> String {
    let mut rendered = String::new();
    for message in messages {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        for block in &message.content {
            let entry = match block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolUse { call } => {
                    format!("called {} {}", call.name, call.arguments)
                }
                ContentBlock::ToolResult { result, .. } => format!(
                    "{}{}",
                    if result.has_error { "error: " } else { "" },
                    result.output_preview
                ),
                ContentBlock::Image { mime_type, .. } => format!("[image {mime_type}]"),
            };
            rendered.push_str(&format!(
                "[{role}] {}\n",
                truncate(&entry, MAX_TRANSCRIPT_ENTRY_CHARS)
            ));
        }
    }
    rendered
}

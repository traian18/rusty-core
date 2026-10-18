//! Reading the latest user message.
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

/// The text of the most recent user message, or empty when there is none.
pub(crate) fn latest_user_text(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

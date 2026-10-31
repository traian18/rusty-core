//! Building the assistant message and tool-use messages in the transcript.

use rusty_protocol::ids::MessageId;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_protocol::tools::ToolCall;

use crate::agent::Agent;

/// Most bytes one assistant message's assembled text may grow to across a run's streamed
/// deltas. Without a cap a runaway or hostile stream would grow the message, held for the whole
/// session, without bound.
pub(super) const MAX_ASSISTANT_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Appended once where an assembled message was cut off.
pub(super) const TRUNCATION_MARKER: &str =
    "\n... (truncated, exceeds assistant message size limit)";

/// Appends `delta` to `text`, stopping (with a one-time truncation marker)
/// once `text` would exceed [`MAX_ASSISTANT_TEXT_BYTES`]. Once truncated,
/// further deltas are silently dropped from the assembled message — the
/// live per-delta `AssistantTextDelta` event still carries the full delta
/// (that event stream is bounded separately, by the runtime's broadcast
/// channel capacity), only the durable, run-lifetime-held assembled text is
/// capped here.
pub(super) fn push_bounded(text: &mut String, delta: &str) {
    if delta.is_empty() {
        return;
    }
    if text.ends_with(TRUNCATION_MARKER) {
        // Already truncated by an earlier delta; further deltas are
        // silently dropped from the assembled message rather than
        // re-appending the marker every time.
        return;
    }
    if text.len() >= MAX_ASSISTANT_TEXT_BYTES {
        text.push_str(TRUNCATION_MARKER);
        return;
    }
    let remaining = MAX_ASSISTANT_TEXT_BYTES - text.len();
    if delta.len() <= remaining {
        text.push_str(delta);
        return;
    }
    // Truncate at a UTF-8 char boundary so we never slice through a
    // multi-byte codepoint.
    let mut cut = remaining;
    while cut > 0 && !delta.is_char_boundary(cut) {
        cut -= 1;
    }
    text.push_str(&delta[..cut]);
    text.push_str(TRUNCATION_MARKER);
}

impl Agent {
    pub(super) fn append_assistant_text(&mut self, delta: &str) -> MessageId {
        // Only the transcript's *last* message may absorb the delta. An
        // earlier assistant message -- one before the user turn that started
        // this run, or before a tool result -- belongs to a different turn,
        // and appending to it would splice the new answer into the old one
        // and leave the current user message with no reply after it.
        if let Some(message) = self.trailing_assistant_message() {
            if let Some(ContentBlock::Text { text }) = message
                .content
                .iter_mut()
                .find(|block| matches!(block, ContentBlock::Text { .. }))
            {
                push_bounded(text, delta);
                return message.id;
            }
            // Opened by a reasoning delta and still empty: give it the text
            // rather than leaving a content-less assistant message behind,
            // which providers reject on the next request.
            if !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
            {
                let mut text = String::new();
                push_bounded(&mut text, delta);
                message.content.push(ContentBlock::Text { text });
                return message.id;
            }
        }

        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        let mut text = String::new();
        push_bounded(&mut text, delta);
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text { text }],
            created_at,
        });
        id
    }

    /// The message a reasoning delta belongs to: the assistant message the
    /// current turn is building, or a fresh one if the turn has produced
    /// nothing else yet. The fresh message starts empty; the text or tool
    /// call that follows fills it, and [`Self::discard_trailing_empty_assistant_message`]
    /// removes it if nothing ever does.
    pub(super) fn assistant_message_id(&mut self) -> MessageId {
        if let Some(message) = self.trailing_assistant_message() {
            return message.id;
        }

        let id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id,
            role: MessageRole::Assistant,
            content: Vec::new(),
            created_at,
        });
        id
    }

    /// Records a tool call the model just requested. Reuses the turn's
    /// assistant message when it has no content yet (opened by a reasoning
    /// delta), so a reasoning-then-tool-call turn does not leave an empty
    /// assistant message ahead of the tool call.
    pub(super) fn push_tool_use(&mut self, call: ToolCall) -> MessageId {
        if let Some(message) = self.trailing_assistant_message() {
            if message.content.is_empty() {
                message.content.push(ContentBlock::ToolUse { call });
                return message.id;
            }
        }
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse { call }],
            created_at,
        });
        message_id
    }

    pub(super) fn trailing_assistant_message(&mut self) -> Option<&mut AgentMessage> {
        self.state
            .messages
            .last_mut()
            .filter(|message| message.role == MessageRole::Assistant)
    }

    /// Drops a trailing assistant message that never received content -- a
    /// turn that produced only reasoning, or nothing at all. Every provider
    /// rejects a content-less assistant message on the next request
    /// (Cohere: "must have non-empty content or tool calls"), and it carries
    /// nothing worth keeping.
    pub(super) fn discard_trailing_empty_assistant_message(&mut self) {
        if self
            .trailing_assistant_message()
            .is_some_and(|message| message.content.is_empty())
        {
            self.state.messages.pop();
        }
    }
}

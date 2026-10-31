//! The cheapest compaction: drop the oldest messages once the history is too long.

use std::sync::Arc;

use async_trait::async_trait;

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::ids::MessageId;
use rusty_protocol::messages::{AgentMessage, ContentBlock};
use rusty_runtime::traits::Workspace;

use crate::compaction::{drop_prefix, elide_tool_results, truncation_note, Retention};

use crate::provider::ContextProvider;

/// Prepends a fixed instruction string to the system prompt — the

/// Wraps another provider; when the transcript's rough character count
/// exceeds `max_chars`, drops all but the most recent `keep_recent` messages
/// and replaces the dropped ones with a single synthetic system note.
///
/// This is deliberately a coarse heuristic, not exact tokenization — exact
/// token counting is provider-specific (a workspace concern this crate
/// doesn't own) and precision here matters less than simply avoiding
/// catastrophic context-window overflow on long sessions. Real LLM-generated
/// summarization of the dropped turns is a natural upgrade once this coarser
/// truncation proves the wiring works end to end.
pub struct TruncatingCompactionProvider {
    inner: Arc<dyn ContextProvider>,
    max_chars: usize,
    keep_recent: usize,
    retention: Retention,
}

impl TruncatingCompactionProvider {
    pub fn new(inner: Arc<dyn ContextProvider>, max_chars: usize, keep_recent: usize) -> Self {
        Self {
            inner,
            max_chars,
            keep_recent,
            retention: Retention::default(),
        }
    }

    /// Messages that must survive compaction regardless of age.
    pub fn with_pinned_messages(mut self, ids: impl IntoIterator<Item = MessageId>) -> Self {
        self.retention.pinned.extend(ids);
        self
    }

    /// Also keep the first user message (the original task) verbatim.
    pub fn with_anchor_first_user_message(mut self, anchor: bool) -> Self {
        self.retention.anchor_first_user = anchor;
        self
    }
}

/// Rough size of one image in the character budget (~1.5k tokens).
pub const IMAGE_CHARS: usize = 6_000;

pub fn message_char_len(message: &AgentMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolResult { result, .. } => result.output_preview.len(),
            // Arguments can be large (a whole file for a write), and the
            // model is sent them on every later turn.
            ContentBlock::ToolUse { call } => call.name.len() + call.arguments.to_string().len(),
            ContentBlock::Image { .. } => IMAGE_CHARS,
        })
        .sum()
}

#[async_trait]
impl ContextProvider for TruncatingCompactionProvider {
    async fn assemble(
        &self,
        request: ExecutionRequest,
        workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let mut request = self.inner.assemble(request, workspace).await;

        let total_chars: usize = request.messages.iter().map(message_char_len).sum();
        if total_chars <= self.max_chars {
            return request;
        }

        // Tier 1: shrink old tool output before discarding any turn.
        elide_tool_results(&mut request.messages, self.keep_recent, &self.retention);
        let total_chars: usize = request.messages.iter().map(message_char_len).sum();
        if total_chars <= self.max_chars {
            return request;
        }

        // Tier 2: drop the oldest turns at a tool-pair-safe cut.
        drop_prefix(
            &mut request.messages,
            self.keep_recent,
            &self.retention,
            truncation_note,
        );
        request
    }
}

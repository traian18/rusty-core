//! A summarizer that asks the execution backend's model.

use super::{SummarizeError, Summarizer, CONTEXT_SUMMARY_PURPOSE};

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use tokio_util::sync::CancellationToken;

use rusty_protocol::backend::{ExecutionEvent, ExecutionParams, ExecutionRequest};
use rusty_protocol::ids::{MessageId, RequestId, RunId, Timestamp};
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use rusty_runtime::traits::ExecutionBackend;

use crate::importance::clip;

pub(super) const SUMMARY_SYSTEM_PROMPT: &str = "You compress the earlier part of a coding assistant's conversation so work can continue from the summary alone. Merge the previous summary (if any) with the new messages into one updated summary. Preserve: the user's goals and constraints, decisions made and why, files created or changed, errors hit and how they were resolved, and open questions. Drop pleasantries and raw tool output; keep exact names, paths and numbers. Reply with the summary only, as terse bullet points.";

/// Summarizes through any [`ExecutionBackend`], so a small cheap model can be
/// pinned with `model`.
pub struct BackendSummarizer {
    backend: Arc<dyn ExecutionBackend>,
    model: Option<String>,
    max_output_tokens: u64,
    max_input_chars: usize,
}

impl BackendSummarizer {
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> Self {
        Self {
            backend,
            model: None,
            max_output_tokens: 1_500,
            max_input_chars: 60_000,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = tokens;
        self
    }
}

/// Plain-text transcript for the summarizer, each message clipped.
pub(super) fn transcript(messages: &[AgentMessage], max_chars: usize) -> String {
    let per_message = (max_chars / messages.len().max(1)).clamp(200, 4_000);
    let mut out = String::new();
    for message in messages {
        let mut body = String::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => body.push_str(text),
                ContentBlock::ToolUse { call } => {
                    body.push_str(&format!("[called {} {}]", call.name, call.arguments))
                }
                ContentBlock::ToolResult { result, .. } => body.push_str(&format!(
                    "[tool result{}] {}",
                    if result.has_error { " (error)" } else { "" },
                    result.output_preview
                )),
                ContentBlock::Image { .. } => body.push_str("[image]"),
            }
            body.push('\n');
        }
        out.push_str(&format!(
            "[{:?}] {}\n\n",
            message.role,
            clip(body.trim(), per_message)
        ));
    }
    out
}

#[async_trait]
impl Summarizer for BackendSummarizer {
    async fn summarize(
        &self,
        previous: Option<&str>,
        messages: &[AgentMessage],
    ) -> Result<String, SummarizeError> {
        let prompt = format!(
            "Previous summary:\n{}\n\nNew messages to fold in:\n{}",
            previous.unwrap_or("(none)"),
            transcript(messages, self.max_input_chars)
        );
        let request = ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: SUMMARY_SYSTEM_PROMPT.to_string(),
            messages: vec![AgentMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: prompt }],
                created_at: Timestamp::now(),
            }],
            tools: vec![],
            extended_thinking: false,
            params: ExecutionParams {
                model: self.model.clone(),
                max_tokens: Some(self.max_output_tokens),
                // Lets hosts tell this apart from the agent's own turns (for
                // example to skip per-step model selection and to account
                // its usage separately).
                provider_options: serde_json::json!({ "rusty": { "purpose": CONTEXT_SUMMARY_PURPOSE } }),
                ..Default::default()
            },
        };

        let (tx, mut rx) = broadcast::channel(256);
        let collector = tokio::spawn(async move {
            let mut text = String::new();
            loop {
                match rx.recv().await {
                    Ok(ExecutionEvent::TextDelta { delta, .. }) => text.push_str(&delta),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            text
        });
        let result = self
            .backend
            .execute(request, tx, CancellationToken::new())
            .await;
        let text = collector.await.unwrap_or_default();
        result.map_err(|e| SummarizeError(format!("summarizer backend failed: {e:?}")))?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(SummarizeError("summarizer returned no text".into()));
        }
        Ok(text)
    }
}

//! Whether a failed attempt may be retried, how long to wait, and how the
//! request is reshaped so a retry continues instead of replaying.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use harness_model::events::{ModelError, ModelResult};
use harness_model::request::ModelRequest;
use harness_protocol::backend::ResponseFormat;
use harness_protocol::ids::{MessageId, Timestamp};
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use super::stream::AttemptProgress;

const CONTINUE_INSTRUCTION: &str = "The response stream was interrupted. Continue exactly where your previous answer stopped. Output only the remaining text, without repeating earlier content or adding an introduction. Use the research and tool results already in this conversation; do not perform further tool calls.";

#[derive(Debug)]
pub(super) enum AttemptDecision {
    Completed(ModelResult),
    Cancelled,
    Retry(ModelError),
    Failed(ModelError),
}

pub(super) struct RetryContext<'a> {
    pub(super) emitted_output: bool,
    pub(super) progress: &'a AttemptProgress,
    pub(super) response_format: &'a Option<ResponseFormat>,
    pub(super) attempt: usize,
    pub(super) max_attempts: usize,
}

pub(super) fn classify_attempt(
    result: Result<ModelResult, ModelError>,
    cancelled: bool,
    context: &RetryContext<'_>,
) -> AttemptDecision {
    match result {
        Ok(result) => AttemptDecision::Completed(result),
        Err(error) if cancelled || matches!(error, ModelError::Cancelled) => {
            AttemptDecision::Cancelled
        }
        Err(error) if should_retry(&error, context) => AttemptDecision::Retry(error),
        Err(error) => AttemptDecision::Failed(error),
    }
}

/// A retry is safe when nothing reached the caller yet, or when only plain
/// text did and the model can be asked to continue it.
fn should_retry(error: &ModelError, context: &RetryContext<'_>) -> bool {
    let plain_text_response = matches!(context.response_format, None | Some(ResponseFormat::Text));
    let can_resume_text = !context.progress.text.is_empty()
        && !context.progress.requested_tools
        && plain_text_response;
    let has_no_output_or_resumable_text = !context.emitted_output || can_resume_text;
    has_no_output_or_resumable_text
        && error.is_retryable()
        && context.attempt < context.max_attempts
}

/// Exponential backoff from 250 ms with up to 50% jitter, unless the provider
/// said how long to wait.
pub(super) fn retry_delay(attempt: usize, error: &ModelError) -> Duration {
    if let Some(delay) = error.retry_after() {
        return delay;
    }
    let base_ms = 250_u64.saturating_mul(1_u64 << (attempt.saturating_sub(1) as u32));
    let jitter_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis() as u64)
        % (base_ms / 2 + 1);
    Duration::from_millis(base_ms + jitter_ms)
}

/// Returns `false` when `cancel` fired before the delay elapsed.
pub(super) async fn wait_retry_delay(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = cancel.cancelled() => false,
    }
}

/// Continue the answer in context, never replay the original request after
/// visible output or repeat side effects.
pub(super) fn append_interrupted_response(model_request: &mut ModelRequest, partial_text: String) {
    for (role, text) in [
        (MessageRole::Assistant, partial_text),
        (MessageRole::User, CONTINUE_INSTRUCTION.to_string()),
    ] {
        model_request.messages.push(AgentMessage {
            id: MessageId::new(),
            role,
            content: vec![ContentBlock::Text { text }],
            created_at: Timestamp::now(),
        });
    }
    model_request.tools.clear();
}

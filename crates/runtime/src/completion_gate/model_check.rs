//! A check that asks a model whether the answer meets the criteria.

use rusty_protocol::backend::{ExecutionEvent, ExecutionParams, ExecutionRequest, ResponseFormat};
use rusty_protocol::commands::VerdictOutcome;
use rusty_protocol::effects::CompletionEvaluationRequest;
use rusty_protocol::ids::{MessageId, RequestId, Timestamp};
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use rusty_protocol::usage::UsageRecord;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::verdict::VERIFIER_PROMPT;
use super::{GateContext, MAX_FEEDBACK_CHARS};

use crate::completion_gate::text::{render_transcript, truncate};
use crate::completion_gate::verdict::{parse_verdict, verdict_schema};

pub(super) async fn ask_model(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    instructions: &str,
    model: Option<String>,
    transcript: &[AgentMessage],
    cancel: CancellationToken,
) -> (VerdictOutcome, Option<UsageRecord>) {
    let structured = context.backend.capabilities().structured_output;
    let mut params = ExecutionParams {
        model: model.or_else(|| context.params.model.clone()),
        ..Default::default()
    };
    if structured {
        params.response_format = Some(ResponseFormat::JsonSchema {
            name: "completion_verdict".into(),
            schema: verdict_schema(),
            strict: true,
        });
    }
    let prompt = format!(
        "<transcript>\n{}</transcript>\n\n<proposed_answer>\n{}\n</proposed_answer>\n\nIs this answer acceptable?",
        render_transcript(transcript),
        truncate(&request.final_response, MAX_FEEDBACK_CHARS * 4),
    );
    let execution = ExecutionRequest {
        request_id: RequestId::new(),
        run_id: request.run_id,
        system_prompt: format!("{VERIFIER_PROMPT}\n\nCriteria:\n{instructions}"),
        messages: vec![AgentMessage {
            id: MessageId::new(),
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: prompt }],
            created_at: Timestamp::now(),
        }],
        tools: Vec::new(),
        extended_thinking: false,
        params,
    };

    let (sink, mut events) = broadcast::channel(4096);
    let record = match context.backend.execute(execution, sink, cancel).await {
        Ok(result) => UsageRecord {
            model_usage: result.usage,
            cost: result.cost,
            tool_usage: None,
        },
        Err(error) => {
            return (
                VerdictOutcome::Error {
                    message: format!("verifier request failed: {error:?}"),
                },
                None,
            )
        }
    };
    let outcome = verdict_from_reply(&mut events);
    (outcome, Some(record))
}

pub(super) fn verdict_from_reply(
    events: &mut broadcast::Receiver<ExecutionEvent>,
) -> VerdictOutcome {
    let mut reply = String::new();
    loop {
        match events.try_recv() {
            Ok(ExecutionEvent::TextDelta { delta, .. }) => reply.push_str(&delta),
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                return VerdictOutcome::Error {
                    message: "verifier reply was too long to read".into(),
                }
            }
            Err(_) => break,
        }
    }
    match parse_verdict(&reply) {
        Ok((true, _)) => VerdictOutcome::Pass,
        Ok((false, feedback)) => VerdictOutcome::Fail {
            feedback: if feedback.trim().is_empty() {
                "The verifier rejected the answer without details.".into()
            } else {
                truncate(&feedback, MAX_FEEDBACK_CHARS)
            },
        },
        Err(message) => VerdictOutcome::Error { message },
    }
}

// ---------------------------------------------------------------------------
// Command evaluator (Claude Code / Codex `Stop` hook contract)
// ---------------------------------------------------------------------------

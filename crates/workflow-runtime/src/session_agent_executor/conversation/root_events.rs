//! What the step does with each event of the session's root agent.

use rusty_protocol::events::{AgentEvent, AgentOutcome};
use rusty_workflow_core::{RetryReason, StructuredOutputMode};
use serde_json::Value;

use super::Conversation;
use crate::orchestration::{AgentExecutionError, AgentStepOutput, StepSignal};
use crate::session_agent_executor::failure::classify_failure;
use crate::session_agent_executor::rate_limit;
use crate::session_agent_executor::reply::parse_reply;

impl Conversation<'_> {
    /// Handles one event of the root agent; `Some` ends the step with its output.
    pub(super) async fn on_root_event(
        &mut self,
        event: AgentEvent,
    ) -> Result<Option<AgentStepOutput>, AgentExecutionError> {
        match event {
            AgentEvent::RunStarted { run_id } => {
                self.delegated.agent_run_id = Some(run_id.to_string());
                self.announce();
            }
            // Only the last message with text is the answer (`AnswerText`).
            AgentEvent::AssistantMessageStarted { .. }
            | AgentEvent::AssistantMessageCompleted { .. } => self.answer.close(),
            AgentEvent::AssistantTextDelta { message_id, delta } => {
                self.answer.delta(message_id, &delta);
            }
            AgentEvent::PermissionRequested { request } => self.permission_requested(request),
            AgentEvent::CompletionGateEvaluated {
                passed: false,
                continuing: false,
                failed_checks,
                ..
            } => self.unpassed_gate = Some(failed_checks),
            AgentEvent::Failed { error } => return Err(self.failed(error).await),
            AgentEvent::Completed { outcome } => return self.completed(outcome).map(Some),
            _ => {}
        }
        Ok(None)
    }

    fn permission_requested(&mut self, permission: rusty_protocol::effects::PermissionRequest) {
        let key = permission.id.to_string();
        self.permissions.insert(key.clone(), permission.id);
        self.context.signal(StepSignal::PermissionRequested {
            permission_id: key,
            tool_name: permission.tool_call.name,
        });
    }

    async fn failed(&mut self, error: rusty_protocol::commands::AgentError) -> AgentExecutionError {
        let failure = classify_failure(error);
        if failure.retry_reason == Some(RetryReason::BackendRateLimited) {
            rate_limit::wait_before_retry(&failure.message, &self.context.cancellation).await;
        }
        failure
    }

    /// The run is over: its last message with text is the answer.
    fn completed(&mut self, outcome: AgentOutcome) -> Result<AgentStepOutput, AgentExecutionError> {
        match outcome {
            AgentOutcome::Success => {}
            AgentOutcome::Cancelled => {
                return Err(AgentExecutionError::new(
                    "cancelled",
                    "isolated agent run was cancelled",
                ))
            }
            other => {
                return Err(AgentExecutionError::new(
                    "agent_attempt_failed",
                    format!("isolated agent completed with {other:?}"),
                ))
            }
        }
        // Inside a workflow step an unpassed gate is a failed attempt: the step's retry policy (or
        // a verify node) decides what happens next.
        if let Some(failed_checks) = self.unpassed_gate.take() {
            return Err(AgentExecutionError::retryable(
                "completion_gate_not_passed",
                format!(
                    "the agent finished without passing its completion gate: {}",
                    failed_checks.join(", ")
                ),
                RetryReason::VerificationFailed,
            ));
        }
        let text = std::mem::take(&mut self.answer).finish();
        self.output_of(text)
    }

    fn output_of(&self, text: String) -> Result<AgentStepOutput, AgentExecutionError> {
        if self.request.structured_output == StructuredOutputMode::Text {
            if text.trim().is_empty() {
                return Err(AgentExecutionError::new(
                    "empty_output",
                    "agent returned no final message",
                ));
            }
            return Ok(AgentStepOutput {
                value: Value::String(text),
            });
        }
        let value = parse_reply(&self.request.output_schema, &text).map_err(|error| {
            AgentExecutionError::retryable(
                "invalid_structured_output",
                error,
                RetryReason::InvalidStructuredOutput,
            )
        })?;
        Ok(AgentStepOutput { value })
    }
}

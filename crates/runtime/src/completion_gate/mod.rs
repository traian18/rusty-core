//! Completion-gate evaluators: the I/O half of the behavior layer's gate.
//!
//! The deterministic core decides *when* a proposed final answer must be
//! checked and what to do with the verdicts; this module produces those
//! verdicts by running a tool or asking a model. Every failure to produce a
//! verdict is reported as [`VerdictOutcome::Error`] — the core applies the
//! check's `error_policy` — so an evaluator can never silently pass.

use std::collections::HashMap;
use std::sync::Arc;

use rusty_agent_core::behavior::EvaluatorSpec;
use rusty_protocol::backend::ExecutionParams;
use rusty_protocol::commands::{CheckVerdict, VerdictOutcome};
use rusty_protocol::effects::{CompletionCheckSpec, CompletionEvaluationRequest};

use rusty_protocol::tools::PermissionMode;
use rusty_protocol::usage::UsageRecord;

use tokio_util::sync::CancellationToken;

use crate::traits::{ExecutionBackend, ToolRegistry, Workspace};

/// Caps on what an evaluator reads back and passes on, so a verbose tool or
/// a long transcript cannot blow up the feedback the model receives.
use command_check::run_command;
use model_check::ask_model;
use text::tail;
use tool_check::{run_tool, with_answer};
use verifier::{run_verifier_agent, VerifierSpec};

mod command_check;
mod model_check;
#[cfg(test)]
mod tests;
mod text;
mod tool_check;
mod usage;
mod verdict;
mod verifier;

const MAX_FEEDBACK_CHARS: usize = 4_000;
const MAX_TRANSCRIPT_ENTRY_CHARS: usize = 2_000;

/// What an evaluation may use from the agent it checks.
pub(crate) struct GateContext {
    pub backend: Arc<dyn ExecutionBackend>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub workspace: Arc<dyn Workspace>,
    /// The host allowed this agent's profiles to run shell commands.
    pub commands_trusted: bool,
    /// Session permission for each enabled tool, by name.
    pub tool_permissions: HashMap<String, PermissionMode>,
    /// The agent's effective execution parameters (its model is the default
    /// for model evaluators).
    pub params: ExecutionParams,
    /// The session of the agent being checked. Tool evaluators run scoped
    /// to it, like the agent's own calls, so an executor serving several
    /// sessions (an orchestration step and its parent) knows whose it is.
    pub session_id: String,
}

/// Run every check in order and return one verdict per check, plus the

/// Run every check in order and return one verdict per check, plus the usage of any model requests
/// the evaluators made.
pub(crate) async fn evaluate(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    cancel: &CancellationToken,
) -> (Vec<CheckVerdict>, Vec<UsageRecord>) {
    let mut verdicts = Vec::with_capacity(request.checks.len());
    let mut usage = Vec::new();
    for check in &request.checks {
        let (outcome, records) = run_check(context, request, check, cancel).await;
        usage.extend(records);
        verdicts.push(CheckVerdict {
            id: check.id.clone(),
            outcome,
        });
        if cancel.is_cancelled() {
            break;
        }
    }
    (verdicts, usage)
}

/// One check: its verdict, and the usage of the model requests it made.
async fn run_check(
    context: &GateContext,
    request: &CompletionEvaluationRequest,
    check: &CompletionCheckSpec,
    cancel: &CancellationToken,
) -> (VerdictOutcome, Vec<UsageRecord>) {
    let spec = match serde_json::from_value::<EvaluatorSpec>(check.evaluator.clone()) {
        Ok(spec) => spec,
        Err(error) => {
            let message = format!("invalid evaluator: {error}");
            return (VerdictOutcome::Error { message }, Vec::new());
        }
    };
    match spec {
        EvaluatorSpec::Tool {
            tool,
            args,
            answer_arg,
        } => {
            let args = with_answer(args, answer_arg.as_deref(), &request.final_response);
            let outcome = rusty_tool_api::with_tool_call(
                format!("gate-{}", check.id),
                context.session_id.clone(),
                run_tool(context, &tool, args, cancel.child_token()),
            )
            .await;
            (outcome, Vec::new())
        }
        EvaluatorSpec::Model {
            instructions,
            model,
            transcript_messages,
        } => {
            let transcript = tail(&request.transcript, transcript_messages as usize);
            let cancel = cancel.child_token();
            let asked = ask_model(context, request, &instructions, model, transcript, cancel);
            let (outcome, record) = asked.await;
            (outcome, record.into_iter().collect())
        }
        EvaluatorSpec::Agent {
            instructions,
            tools,
            model,
            max_turns,
            transcript_messages,
        } => {
            let transcript = tail(&request.transcript, transcript_messages as usize);
            let spec = VerifierSpec {
                instructions: &instructions,
                tools,
                model,
                max_turns,
            };
            run_verifier_agent(context, request, spec, transcript, cancel.child_token()).await
        }
        EvaluatorSpec::Command {
            command,
            timeout_ms,
        } => {
            let outcome = run_command(context, request, &command, timeout_ms, cancel).await;
            (outcome, Vec::new())
        }
    }
}

//! Turning an event-streamed harness run into one request/response answer.
//!
//! MCP's `tools/call` is synchronous: the caller sends arguments and blocks
//! for a result. The harness is the opposite shape — a mutation is *admitted*
//! and the actual work surfaces as a stream of `AgentEventEnvelope`s. This
//! module bridges the two, and is where essentially all of this crate's
//! subtlety lives.

use std::sync::Arc;
use std::time::Duration;

use harness_protocol::admission::{AdmissionResult, CommandId, MutationMetadata};
use harness_protocol::commands::UserInput;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome};
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{MutationCommand, RpcRequestBody, RpcResponseBody};
use harness_runtime::rpc::RpcHandler;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Everything a finished run produced that an MCP caller cares about.
#[derive(Debug, Default)]
pub(crate) struct RunTranscript {
    pub text: String,
    pub tool_calls: Vec<String>,
}

impl RunTranscript {
    /// Folds one event in. Shared by the live drain and the `events_since`
    /// replay so both render a run identically.
    fn absorb(&mut self, event: &AgentEvent) {
        match event {
            // The *only* source of assistant text — there is no
            // message-bearing snapshot RPC to fall back on.
            AgentEvent::AssistantTextDelta { delta, .. } => self.text.push_str(delta),
            AgentEvent::ToolCallCompleted { result, .. } => {
                let marker = if result.has_error { "error" } else { "ok" };
                self.tool_calls
                    .push(format!("[{marker}] {}", result.output_preview));
            }
            _ => {}
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tool_calls.is_empty()
    }
}

/// How a run ended, from this transport's point of view.
#[derive(Debug)]
pub(crate) enum RunEnd {
    Completed(AgentOutcome),
    Failed(String),
    /// A permission prompt arrived with nobody able to answer it. See the
    /// note in [`drain`].
    PermissionBlocked,
    TimedOut(Duration),
    /// The session closed, or the event channel dropped, before the run
    /// reported an outcome.
    Closed,
    Cancelled,
}

impl RunEnd {
    /// Whether the MCP result should be flagged as an error.
    pub(crate) fn is_error(&self) -> bool {
        !matches!(self, RunEnd::Completed(AgentOutcome::Success))
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            RunEnd::Completed(AgentOutcome::Success) => "completed".to_owned(),
            RunEnd::Completed(AgentOutcome::Cancelled) => "the run was cancelled".to_owned(),
            RunEnd::Completed(AgentOutcome::Failed) => "the run failed".to_owned(),
            RunEnd::Failed(message) => format!("the agent failed: {message}"),
            RunEnd::PermissionBlocked => {
                "the run stopped at a tool-permission prompt, which cannot be answered over \
                 MCP. Configure this server with an all-Allow toolset."
                    .to_owned()
            }
            RunEnd::TimedOut(after) => format!("timed out after {after:?}"),
            RunEnd::Closed => "the session closed before the run reported an outcome".to_owned(),
            RunEnd::Cancelled => "the client cancelled the call".to_owned(),
        }
    }
}

/// Sends `input` to `session_id` and blocks until its run finishes.
///
/// **Subscribe-before-mutate is load-bearing.** `RpcHandler::subscribe`
/// hands back a `broadcast::Receiver`, which only delivers messages sent
/// after it exists. Subscribing after the prompt is admitted races the run
/// and silently drops its opening events — including, on a fast run, the
/// `Completed` that this function is waiting for, which would then hang
/// until the timeout. The `Subscribe` dispatch in `harness-transport-stdio`
/// establishes the same ordering.
pub(crate) async fn prompt_and_wait(
    handler: &Arc<dyn RpcHandler>,
    session_id: SessionId,
    input: UserInput,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(RunTranscript, RunEnd), String> {
    // 1. Subscribe first. See the note above.
    let Some(receiver) = handler.subscribe(session_id) else {
        return Err(format!("session {session_id} is not open"));
    };

    // 2. Admit the mutation.
    let admission = handler
        .handle(
            Some(session_id),
            RpcRequestBody::Mutate {
                metadata: MutationMetadata {
                    command_id: CommandId::new(),
                    session_id,
                    run_id: None,
                    // Deliberately no optimistic-concurrency check: an MCP
                    // client has no way to observe session revisions, and
                    // the handler only enforces this when a caller supplies
                    // one.
                    expected_session_revision: None,
                    trace_id: None,
                },
                command: MutationCommand::Prompt(input),
            },
        )
        .await;

    match admission {
        RpcResponseBody::Admission { result, .. } if accepted(&result) => {}
        RpcResponseBody::Admission { result, .. } => {
            // Never started, so there is nothing to wait for — returning
            // now beats blocking until the timeout.
            return Err(format!("the prompt was not accepted: {result:?}"));
        }
        RpcResponseBody::Failure(error) => return Err(error.message),
        other => return Err(format!("unexpected response to a prompt: {other:?}")),
    }

    // 3. Drain to completion.
    Ok(drain(handler, session_id, receiver, timeout, cancel).await)
}

fn accepted(result: &AdmissionResult) -> bool {
    match result {
        AdmissionResult::Accepted
        | AdmissionResult::AcceptedApplied
        | AdmissionResult::AcceptedStarted { .. }
        | AdmissionResult::AcceptedQueued { .. } => true,
        // A duplicate command id means the original was already admitted;
        // whether *that* counts depends on what it was.
        AdmissionResult::Duplicate { original } => accepted(original),
        AdmissionResult::RejectedConflict { .. }
        | AdmissionResult::RejectedClosed
        | AdmissionResult::RejectedInvalidState { .. }
        | AdmissionResult::RejectedCapacity { .. } => false,
    }
}

async fn drain(
    handler: &Arc<dyn RpcHandler>,
    session_id: SessionId,
    mut receiver: tokio::sync::broadcast::Receiver<AgentEventEnvelope>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (RunTranscript, RunEnd) {
    let mut transcript = RunTranscript::default();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        let envelope = tokio::select! {
            _ = cancel.cancelled() => return (transcript, RunEnd::Cancelled),
            _ = &mut deadline => return (transcript, RunEnd::TimedOut(timeout)),
            received = receiver.recv() => received,
        };

        match envelope {
            Ok(envelope) => {
                transcript.absorb(&envelope.event);
                match envelope.event {
                    AgentEvent::Completed { outcome } => {
                        return (transcript, RunEnd::Completed(outcome))
                    }
                    AgentEvent::Failed { error } => {
                        return (transcript, RunEnd::Failed(error.message))
                    }
                    // A permission prompt parks the run indefinitely: there
                    // is no MCP-side channel to answer it, so waiting would
                    // burn the full timeout and then report something
                    // misleading. Say what actually happened instead.
                    AgentEvent::PermissionRequested { request } => {
                        warn!(
                            session = %session_id,
                            tool = %request.tool_call.name,
                            "MCP: run blocked on a permission prompt that cannot be answered"
                        );
                        return (transcript, RunEnd::PermissionBlocked);
                    }
                    _ => {}
                }
            }
            // The subscriber fell behind and the channel dropped events.
            // Rather than return a silently truncated transcript, rebuild
            // from the durable store — the same "a gap is a first-class
            // signal, never a silent hole" stance behind the RPC contract's
            // `EventGap`.
            Err(RecvError::Lagged(dropped)) => {
                debug!(session = %session_id, dropped, "MCP: subscriber lagged, replaying");
                return replay(handler, session_id).await;
            }
            Err(RecvError::Closed) => return (transcript, RunEnd::Closed),
        }
    }
}

/// Rebuilds a run's transcript from every durable event, used when the live
/// subscription lagged.
async fn replay(handler: &Arc<dyn RpcHandler>, session_id: SessionId) -> (RunTranscript, RunEnd) {
    let mut transcript = RunTranscript::default();
    let mut end = RunEnd::Closed;

    for envelope in handler.events_since(session_id, 0).await {
        transcript.absorb(&envelope.event);
        match envelope.event {
            AgentEvent::Completed { outcome } => end = RunEnd::Completed(outcome),
            AgentEvent::Failed { error } => end = RunEnd::Failed(error.message),
            _ => {}
        }
    }
    (transcript, end)
}

/// Renders every durable event for a session as a readable transcript, for
/// the `harness://session/{id}` resource.
///
/// This is why MCP resources need no protocol addition: `SessionSnapshotWire`
/// carries only status and usage, but `events_since(id, 0)` already returns
/// the full durable history.
pub(crate) async fn render_transcript(
    handler: &Arc<dyn RpcHandler>,
    session_id: SessionId,
) -> String {
    let mut out = String::new();
    for envelope in handler.events_since(session_id, 0).await {
        match envelope.event {
            AgentEvent::AssistantTextDelta { delta, .. } => out.push_str(&delta),
            AgentEvent::ToolCallRequested { call } => {
                out.push_str(&format!("\n\n[tool: {}]\n", call.name));
            }
            AgentEvent::ToolCallCompleted { result, .. } => {
                let marker = if result.has_error { "error" } else { "ok" };
                out.push_str(&format!("[{marker}] {}\n", result.output_preview));
            }
            AgentEvent::Completed { outcome } => {
                out.push_str(&format!("\n\n[run {outcome:?}]\n"));
            }
            AgentEvent::Failed { error } => {
                out.push_str(&format!("\n\n[failed: {}]\n", error.message));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use harness_protocol::ids::RunId;
    use harness_protocol::tools::ToolResultSummary;

    #[test]
    fn accepted_covers_every_success_variant_and_no_rejection() {
        assert!(accepted(&AdmissionResult::Accepted));
        assert!(accepted(&AdmissionResult::AcceptedApplied));
        assert!(accepted(&AdmissionResult::AcceptedStarted {
            run_id: RunId::new()
        }));
        assert!(accepted(&AdmissionResult::AcceptedQueued {
            run_id: RunId::new(),
            position: 2
        }));

        assert!(!accepted(&AdmissionResult::RejectedClosed));
        assert!(!accepted(&AdmissionResult::RejectedConflict {
            current_session_revision: 3
        }));
        assert!(!accepted(&AdmissionResult::RejectedInvalidState {
            reason: "busy".into()
        }));
        assert!(!accepted(&AdmissionResult::RejectedCapacity {
            limit: "queue".into()
        }));
    }

    /// A duplicate must be judged by what the *original* command did, not
    /// treated as a blanket success.
    #[test]
    fn a_duplicate_inherits_the_verdict_of_its_original() {
        assert!(accepted(&AdmissionResult::Duplicate {
            original: Box::new(AdmissionResult::Accepted)
        }));
        assert!(!accepted(&AdmissionResult::Duplicate {
            original: Box::new(AdmissionResult::RejectedClosed)
        }));
    }

    #[test]
    fn transcript_accumulates_text_and_flags_failing_tool_calls() {
        let mut transcript = RunTranscript::default();
        transcript.absorb(&AgentEvent::AssistantTextDelta {
            message_id: harness_protocol::ids::MessageId::new(),
            delta: "Hello ".into(),
        });
        transcript.absorb(&AgentEvent::AssistantTextDelta {
            message_id: harness_protocol::ids::MessageId::new(),
            delta: "world".into(),
        });
        transcript.absorb(&AgentEvent::ToolCallCompleted {
            call_id: harness_protocol::ids::ToolCallId::new(),
            result: ToolResultSummary {
                has_error: true,
                output_preview: "boom".into(),
            },
        });

        assert_eq!(transcript.text, "Hello world");
        assert_eq!(transcript.tool_calls, vec!["[error] boom".to_string()]);
        assert!(!transcript.is_empty());
    }

    #[test]
    fn only_a_successful_completion_is_not_an_error() {
        assert!(!RunEnd::Completed(AgentOutcome::Success).is_error());
        assert!(RunEnd::Completed(AgentOutcome::Cancelled).is_error());
        assert!(RunEnd::Completed(AgentOutcome::Failed).is_error());
        assert!(RunEnd::Failed("x".into()).is_error());
        assert!(RunEnd::PermissionBlocked.is_error());
        assert!(RunEnd::TimedOut(Duration::from_secs(1)).is_error());
        assert!(RunEnd::Closed.is_error());
        assert!(RunEnd::Cancelled.is_error());
    }

    /// The permission message must name the cause and the fix — a caller
    /// seeing only "timed out" would have no idea what went wrong.
    #[test]
    fn the_permission_block_message_explains_the_remedy() {
        let text = RunEnd::PermissionBlocked.describe();
        assert!(text.contains("permission"), "{text}");
        assert!(text.contains("Allow"), "{text}");
    }
}

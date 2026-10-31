//! The channel between the runner and an executor for one attempt: signals out, decisions in.

use rusty_protocol::{commands::PermissionDecision, events::AgentEventEnvelope};
use rusty_workflow_core::{
    DelegatedRunRef, InputRequest, InputResponse, OrchestrationNodeId, OrchestrationRunId,
    UsageSummary,
};
use serde_json::Value;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::orchestration::steps::request::AgentExecutionError;

/// Observations an executor reports while an attempt is in flight.
#[derive(Debug)]
pub enum StepSignal {
    Checkpoint {
        value: Value,
        committed: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// The session/agent/run executing the attempt. Sent as soon as known so
    /// even failed or cancelled attempts are correlated.
    Delegated(DelegatedRunRef),
    /// The delegated agent is blocked on a tool permission decision.
    PermissionRequested {
        permission_id: String,
        tool_name: String,
    },
    /// Cumulative usage of the attempt so far (replaces earlier reports).
    Usage(UsageSummary),
    /// A raw event from the delegated agent, republished with correlation.
    Agent(Box<AgentEventEnvelope>),
    /// The attempt asks the user a question and waits for the answer (see
    /// [`StepContext::ask`]).
    InputRequested(InputRequest),
}

#[derive(Debug, Clone)]
pub struct PermissionResolution {
    pub permission_id: String,
    pub decision: PermissionDecision,
}

/// The user's answer to a question an attempt asked.
#[derive(Debug, Clone)]
pub struct InputAnswer {
    pub request_id: String,
    pub response: InputResponse,
}

/// Per-attempt channel between the runner and an executor.
pub struct StepContext {
    pub cancellation: CancellationToken,
    signals: mpsc::UnboundedSender<StepSignal>,
    /// Decisions for permissions this attempt reported.
    pub permissions: mpsc::UnboundedReceiver<PermissionResolution>,
    answers: mpsc::UnboundedReceiver<InputAnswer>,
}

impl StepContext {
    pub(crate) fn new(
        cancellation: CancellationToken,
        signals: mpsc::UnboundedSender<StepSignal>,
        permissions: mpsc::UnboundedReceiver<PermissionResolution>,
        answers: mpsc::UnboundedReceiver<InputAnswer>,
    ) -> Self {
        Self {
            cancellation,
            signals,
            permissions,
            answers,
        }
    }

    /// A context whose signals go nowhere and that never receives decisions.
    pub fn detached(cancellation: CancellationToken) -> Self {
        let (signals, _) = mpsc::unbounded_channel();
        let (_, permissions) = mpsc::unbounded_channel();
        let (_, answers) = mpsc::unbounded_channel();
        Self::new(cancellation, signals, permissions, answers)
    }

    /// Ask the user `request` and wait for the answer. The run shows as
    /// waiting for input meanwhile, and the wait does not count as a stall.
    pub async fn ask(
        &mut self,
        request: InputRequest,
    ) -> Result<InputResponse, AgentExecutionError> {
        let id = request.id.clone();
        self.signals
            .send(StepSignal::InputRequested(request))
            .map_err(|_| AgentExecutionError::new("cancelled", "workflow stopped before asking"))?;
        loop {
            tokio::select! {
                _ = self.cancellation.cancelled() => {
                    return Err(AgentExecutionError::new("cancelled", "workflow cancelled while waiting for an answer"));
                }
                answer = self.answers.recv() => match answer {
                    Some(answer) if answer.request_id == id => return Ok(answer.response),
                    Some(_) => continue,
                    None => return Err(AgentExecutionError::new("cancelled", "workflow stopped while waiting for an answer")),
                },
            }
        }
    }

    pub async fn checkpoint(&self, value: Value) -> Result<(), AgentExecutionError> {
        let (committed, ack) = tokio::sync::oneshot::channel();
        self.signals
            .send(StepSignal::Checkpoint { value, committed })
            .map_err(|_| {
                AgentExecutionError::new("checkpoint_failed", "workflow stopped before checkpoint")
            })?;
        ack.await
            .map_err(|_| {
                AgentExecutionError::new("checkpoint_failed", "checkpoint was not committed")
            })?
            .map_err(|error| AgentExecutionError::new("checkpoint_failed", error))
    }

    pub fn signal(&self, signal: StepSignal) {
        // The runner may have stopped listening (e.g. cancellation); signals
        // are observations, so dropping them then is harmless.
        let _ = self.signals.send(signal);
    }
}

/// A child run a `subflow` node asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct SubflowRequest {
    pub run_id: OrchestrationRunId,
    pub node_id: OrchestrationNodeId,
    pub attempt: u32,
    pub target: rusty_workflow_core::SubflowTarget,
    /// The values bound to the node: the child's run input.
    pub input: Value,
    /// How deep the child is: a flow run on its own is level 0.
    pub depth: u32,
    pub options: rusty_workflow_core::RunOptions,
}

//! The caller's side of a running orchestration: observe, control, wait.

use harness_core::orchestration::{InputResponse, OrchestrationRunId, OrchestrationRunState};
use harness_protocol::commands::PermissionDecision;
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{ControlError, OrchestrationRunOutput, OrchestrationRuntimeError, OrchestrationUpdate};

/// What a caller asks the run loop to do.
pub(super) enum ControlRequest {
    Pause,
    Resume,
    ResolvePermission {
        permission_id: String,
        decision: PermissionDecision,
    },
    ResolveInput {
        request_id: String,
        response: InputResponse,
    },
}

/// A request plus the channel its outcome goes back on.
pub(super) struct Control {
    pub(super) request: ControlRequest,
    pub(super) reply: oneshot::Sender<Result<(), ControlError>>,
}

type RunJoin = JoinHandle<Result<OrchestrationRunOutput, OrchestrationRuntimeError>>;

/// Control and observation handle for one running orchestration.
pub struct OrchestrationHandle {
    run_id: OrchestrationRunId,
    control: mpsc::UnboundedSender<Control>,
    updates: broadcast::Sender<OrchestrationUpdate>,
    first_subscriber: Option<broadcast::Receiver<OrchestrationUpdate>>,
    state: watch::Receiver<OrchestrationRunState>,
    cancellation: CancellationToken,
    join: RunJoin,
}

impl OrchestrationHandle {
    pub(super) fn new(
        run_id: OrchestrationRunId,
        control: mpsc::UnboundedSender<Control>,
        updates: broadcast::Sender<OrchestrationUpdate>,
        first_subscriber: broadcast::Receiver<OrchestrationUpdate>,
        state: watch::Receiver<OrchestrationRunState>,
        cancellation: CancellationToken,
        join: RunJoin,
    ) -> Self {
        Self {
            run_id,
            control,
            updates,
            first_subscriber: Some(first_subscriber),
            state,
            cancellation,
            join,
        }
    }

    pub fn run_id(&self) -> &OrchestrationRunId {
        &self.run_id
    }

    /// Subscribe to updates. The first call receives every update since the
    /// run started; later calls receive updates from the moment they
    /// subscribe.
    pub fn subscribe(&mut self) -> broadcast::Receiver<OrchestrationUpdate> {
        self.first_subscriber
            .take()
            .unwrap_or_else(|| self.updates.subscribe())
    }

    /// Current run and step state, without parsing any chat text.
    pub fn snapshot(&self) -> OrchestrationRunState {
        self.state.borrow().clone()
    }

    /// Watch state changes (one per committed transition).
    pub fn watch(&self) -> watch::Receiver<OrchestrationRunState> {
        self.state.clone()
    }

    /// Stop admitting new steps. A step already running finishes first.
    pub async fn pause(&self) -> Result<(), ControlError> {
        self.request(ControlRequest::Pause).await
    }

    pub async fn resume(&self) -> Result<(), ControlError> {
        self.request(ControlRequest::Resume).await
    }

    /// Cancel the run, propagating to the active delegated session.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Resolve a permission requested by the active step's delegated agent.
    pub async fn resolve_permission(
        &self,
        permission_id: impl Into<String>,
        decision: PermissionDecision,
    ) -> Result<(), ControlError> {
        let permission_id = permission_id.into();
        self.request(ControlRequest::ResolvePermission {
            permission_id,
            decision,
        })
        .await
    }

    /// Answer the question the active step asked (an `InputRequested`
    /// event). An answer that is not one of the offered decisions, or lacks
    /// required text, is rejected and the step keeps waiting.
    pub async fn resolve_input(
        &self,
        request_id: impl Into<String>,
        response: InputResponse,
    ) -> Result<(), ControlError> {
        let request_id = request_id.into();
        self.request(ControlRequest::ResolveInput {
            request_id,
            response,
        })
        .await
    }

    async fn request(&self, request: ControlRequest) -> Result<(), ControlError> {
        let (reply, response) = oneshot::channel();
        self.control
            .send(Control { request, reply })
            .map_err(|_| ControlError::Closed)?;
        response.await.map_err(|_| ControlError::Closed)?
    }

    /// Wait for the run to reach a terminal state.
    pub async fn wait(self) -> Result<OrchestrationRunOutput, OrchestrationRuntimeError> {
        self.join
            .await
            .map_err(|error| OrchestrationRuntimeError::TaskFailed(error.to_string()))?
    }
}

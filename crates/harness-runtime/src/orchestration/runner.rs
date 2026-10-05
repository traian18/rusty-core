//! The async orchestration runner.
//!
//! One spawned task owns a run. It feeds commands to the deterministic
//! reducer in `harness-core`, commits every resulting transition to the
//! [`OrchestrationStore`] *before* publishing events or dispatching effects,
//! executes at most one step at a time, and exposes control through an
//! [`OrchestrationHandle`].

use std::{
    future::pending,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use harness_core::orchestration::{
    apply, check_restorable, error_codes, ready_steps, AttemptDetails, CompiledOrchestration,
    DelegatedRunRef, OrchestrationCommand, OrchestrationDefinitionId, OrchestrationEffect,
    OrchestrationError, OrchestrationNode, OrchestrationNodeId, OrchestrationNodeKind,
    OrchestrationResult, OrchestrationRunId, OrchestrationRunState, OrchestrationStatus,
    RetryReason, StepStatus, ToolScope, TransitionError, UsageSummary,
};
use harness_protocol::{commands::PermissionDecision, events::AgentEventEnvelope};
use serde_json::Value;
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{
    schema::{BasicSchemaValidator, InMemorySchemaResolver, SchemaResolver, SchemaValidator},
    steps::{
        execute_input, execute_verify, resolve_binding, resolve_inputs, AgentStepExecutor,
        AgentStepRequest, ArtifactResolver, PermissionResolution, ReferenceArtifactResolver,
        StepContext, StepSignal, Validation,
    },
    store::{
        OrchestrationEventEnvelope, OrchestrationSnapshot, OrchestrationStore,
        OrchestrationStoreError,
    },
};

const UPDATE_CAPACITY: usize = 1024;

/// How long a cancelled executor may take to wind down its delegated work
/// (cancel the run, release the session) before its future is dropped.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Everything needed to attribute an agent event to a workflow step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCorrelation {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub node_id: OrchestrationNodeId,
    pub attempt: u32,
    pub delegated: DelegatedRunRef,
}

#[derive(Debug, Clone)]
pub struct CorrelatedAgentEvent {
    pub correlation: StepCorrelation,
    pub envelope: AgentEventEnvelope,
}

/// What subscribers receive: committed orchestration events, and live agent
/// events from delegated attempts tagged with their step correlation.
#[derive(Debug, Clone)]
pub enum OrchestrationUpdate {
    Event(OrchestrationEventEnvelope),
    Agent(Box<CorrelatedAgentEvent>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrchestrationRunOutput {
    pub state: OrchestrationRunState,
    pub result: OrchestrationResult,
    /// Events committed by this runner invocation (a resumed run starts
    /// with `RunRestored`; the store holds the full log).
    pub events: Vec<OrchestrationEventEnvelope>,
}

impl OrchestrationRunOutput {
    /// Delegated session/agent/run for every agent attempt, including failed
    /// and cancelled ones.
    pub fn agent_correlations(&self) -> Vec<StepCorrelation> {
        self.state
            .steps
            .iter()
            .flat_map(|(node_id, step)| {
                step.attempts.iter().filter_map(move |attempt| {
                    attempt
                        .details
                        .delegated
                        .clone()
                        .map(|delegated| StepCorrelation {
                            run_id: self.state.run_id.clone(),
                            definition_id: self.state.definition_id.clone(),
                            revision: self.state.definition_revision,
                            node_id: node_id.clone(),
                            attempt: attempt.attempt,
                            delegated,
                        })
                })
            })
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum OrchestrationRuntimeError {
    #[error("orchestration transition failed ({code}): {message}")]
    Transition { code: &'static str, message: String },
    #[error("persistence failed at the commit boundary; run halted: {0}")]
    Persistence(#[from] OrchestrationStoreError),
    #[error("definition references tools that are not registered: {0:?}")]
    UnknownTools(Vec<String>),
    #[error("resuming a run requires an orchestration store")]
    NoStore,
    #[error("run {0} has no snapshot to resume from")]
    RunNotFound(OrchestrationRunId),
    #[error("run {0} already finished")]
    RunFinished(OrchestrationRunId),
    #[error("cannot restore run: {0}")]
    Restore(String),
    #[error("orchestration stalled in status {0:?} with no runnable step")]
    Stalled(OrchestrationStatus),
    #[error("orchestration task ended abnormally: {0}")]
    TaskFailed(String),
}

impl From<TransitionError> for OrchestrationRuntimeError {
    fn from(error: TransitionError) -> Self {
        Self::Transition {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    #[error("{0}")]
    Rejected(String),
    #[error("the orchestration run is no longer accepting commands")]
    Closed,
}

enum Control {
    Pause(oneshot::Sender<Result<(), ControlError>>),
    Resume(oneshot::Sender<Result<(), ControlError>>),
    ResolvePermission {
        permission_id: String,
        decision: PermissionDecision,
        reply: oneshot::Sender<Result<(), ControlError>>,
    },
}

/// Control and observation handle for one running orchestration.
pub struct OrchestrationHandle {
    run_id: OrchestrationRunId,
    control: mpsc::UnboundedSender<Control>,
    updates: broadcast::Sender<OrchestrationUpdate>,
    first_subscriber: Option<broadcast::Receiver<OrchestrationUpdate>>,
    state: watch::Receiver<OrchestrationRunState>,
    cancellation: CancellationToken,
    join: JoinHandle<Result<OrchestrationRunOutput, OrchestrationRuntimeError>>,
}

impl OrchestrationHandle {
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
        self.request(Control::Pause).await
    }

    pub async fn resume(&self) -> Result<(), ControlError> {
        self.request(Control::Resume).await
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
        self.request(|reply| Control::ResolvePermission {
            permission_id,
            decision,
            reply,
        })
        .await
    }

    async fn request(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<(), ControlError>>) -> Control,
    ) -> Result<(), ControlError> {
        let (reply, response) = oneshot::channel();
        self.control
            .send(make(reply))
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

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Executes one compiled definition. Cheap to clone.
#[derive(Clone)]
pub struct OrchestrationRunner {
    compiled: Arc<CompiledOrchestration>,
    agent: Arc<dyn AgentStepExecutor>,
    schemas: Arc<dyn SchemaResolver>,
    validator: Arc<dyn SchemaValidator>,
    artifacts: Arc<dyn ArtifactResolver>,
    store: Option<Arc<dyn OrchestrationStore>>,
    available_tools: Option<Arc<Vec<String>>>,
}

impl OrchestrationRunner {
    pub fn new(compiled: Arc<CompiledOrchestration>, agent: Arc<dyn AgentStepExecutor>) -> Self {
        Self {
            compiled,
            agent,
            schemas: Arc::new(InMemorySchemaResolver::new()),
            validator: Arc::new(BasicSchemaValidator),
            artifacts: Arc::new(ReferenceArtifactResolver),
            store: None,
            available_tools: None,
        }
    }

    pub fn with_schemas(mut self, schemas: Arc<dyn SchemaResolver>) -> Self {
        self.schemas = schemas;
        self
    }

    pub fn with_validator(mut self, validator: Arc<dyn SchemaValidator>) -> Self {
        self.validator = validator;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<dyn ArtifactResolver>) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// Persist events and snapshots, enabling [`resume`](Self::resume).
    pub fn with_store(mut self, store: Arc<dyn OrchestrationStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// The host's registered tools. Allow-listed tools are checked against it
    /// before a run starts, and `ToolScope::Inherit` resolves to it. Without
    /// it, `Inherit` fails closed.
    pub fn with_available_tools(mut self, tools: impl IntoIterator<Item = String>) -> Self {
        let mut tools: Vec<_> = tools.into_iter().collect();
        tools.sort();
        tools.dedup();
        self.available_tools = Some(Arc::new(tools));
        self
    }

    pub fn compiled(&self) -> &Arc<CompiledOrchestration> {
        &self.compiled
    }

    /// Start a new run. Fails without side effects if the definition
    /// references unregistered tools.
    pub fn start(
        &self,
        run_id: OrchestrationRunId,
        input: Value,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        self.check_tool_references()?;
        let state = OrchestrationRunState::new(run_id, &self.compiled);
        Ok(self.spawn(
            state,
            0,
            0,
            OrchestrationCommand::Start { input },
            CancellationToken::new(),
        ))
    }

    /// Resume a run from its last committed snapshot. Completed steps are not
    /// re-run; an agent attempt that was in flight fails closed with
    /// `INDETERMINATE_ATTEMPT` because its side effects are unknown.
    pub async fn resume(
        &self,
        run_id: OrchestrationRunId,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        let store = self
            .store
            .as_ref()
            .ok_or(OrchestrationRuntimeError::NoStore)?;
        let snapshot = store
            .load_snapshot(&run_id)
            .await?
            .ok_or_else(|| OrchestrationRuntimeError::RunNotFound(run_id.clone()))?;
        if snapshot.state.status.is_terminal() {
            return Err(OrchestrationRuntimeError::RunFinished(run_id));
        }
        check_restorable(&self.compiled, &snapshot.state)
            .map_err(|error| OrchestrationRuntimeError::Restore(error.message))?;
        self.check_tool_references()?;
        Ok(self.spawn(
            snapshot.state,
            snapshot.last_sequence,
            snapshot.elapsed_ms,
            OrchestrationCommand::Recover,
            CancellationToken::new(),
        ))
    }

    /// Explicit user retry from a failed checkpoint. Preserve outputs and attempt
    /// history; never replay successful steps or silently change definitions.
    pub fn retry_failed(
        &self,
        mut state: OrchestrationRunState,
        run_id: OrchestrationRunId,
        guidance: String,
    ) -> Result<OrchestrationHandle, OrchestrationRuntimeError> {
        check_restorable(&self.compiled, &state)?;
        self.check_tool_references()?;
        if state.status != OrchestrationStatus::Failed {
            return Err(OrchestrationRuntimeError::Restore(
                "only failed runs can be retried".into(),
            ));
        }
        let node_id = state.failed_step.take().ok_or_else(|| {
            OrchestrationRuntimeError::Restore("no failed step checkpoint is available".into())
        })?;
        let step = state
            .steps
            .get(&node_id)
            .ok_or_else(|| OrchestrationRuntimeError::Restore("failed step is missing".into()))?;
        if step.status != StepStatus::Failed {
            return Err(OrchestrationRuntimeError::Restore(
                "checkpoint step is not failed".into(),
            ));
        }
        let repair_target = match self.compiled.node(&node_id).map(|node| &node.kind) {
            Some(OrchestrationNodeKind::Verify(config))
                if state
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == "verification_failed") =>
            {
                config.retry_target.clone()
            }
            _ => None,
        };
        let error = state
            .steps
            .get_mut(&node_id)
            .and_then(|step| step.error.take());
        let resumed_node = if let Some(target_id) = repair_target {
            // An explicit continuation grants the repair target another attempt after its
            // automatic retries have run out. Clear downstream results so the
            // next review checks the repaired workspace, not stale output.
            for span_node in &self.compiled.retry_spans[&node_id] {
                let step = state
                    .steps
                    .get_mut(span_node)
                    .expect("retry span step exists");
                step.status = StepStatus::Pending;
                step.output = None;
                step.pending_permissions.clear();
                if span_node != &target_id {
                    step.checkpoint = None;
                }
            }
            target_id
        } else {
            node_id
        };
        let step = state
            .steps
            .get_mut(&resumed_node)
            .expect("resumed step exists");
        if let Some(error) = error {
            step.feedback.push(error);
        }
        step.feedback.push(OrchestrationError::new("USER_CONTINUATION", format!(
            "Resume this step. Earlier successful steps and their outputs are retained. The failed attempt may have changed the workspace: inspect current state before repeating actions. User guidance: {guidance}"
        )));
        step.status = StepStatus::Ready;
        step.pending_permissions.clear();
        for step in state.steps.values_mut() {
            if step.status == StepStatus::Skipped {
                step.status = StepStatus::Pending;
            }
        }
        state.run_id = run_id;
        state.status = OrchestrationStatus::Ready;
        state.paused = false;
        state.error = None;
        state.final_output = None;
        Ok(self.spawn(
            state,
            0,
            0,
            OrchestrationCommand::Recover,
            CancellationToken::new(),
        ))
    }

    /// Start a run and wait for it, cancelling when `cancellation` fires.
    pub async fn run(
        &self,
        run_id: OrchestrationRunId,
        input: Value,
        cancellation: CancellationToken,
    ) -> Result<OrchestrationRunOutput, OrchestrationRuntimeError> {
        self.check_tool_references()?;
        let state = OrchestrationRunState::new(run_id, &self.compiled);
        self.spawn(
            state,
            0,
            0,
            OrchestrationCommand::Start { input },
            cancellation,
        )
        .wait()
        .await
    }

    fn check_tool_references(&self) -> Result<(), OrchestrationRuntimeError> {
        let Some(available) = &self.available_tools else {
            return Ok(());
        };
        let mut missing: Vec<_> = self
            .compiled
            .nodes
            .values()
            .filter_map(|node| match &node.kind {
                OrchestrationNodeKind::Agent(config) => match &config.tools {
                    ToolScope::AllowList(tools) => Some(tools),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .filter(|tool| available.binary_search(tool).is_err())
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(OrchestrationRuntimeError::UnknownTools(missing))
        }
    }

    fn spawn(
        &self,
        state: OrchestrationRunState,
        last_sequence: u64,
        elapsed_before_ms: u64,
        first: OrchestrationCommand,
        cancellation: CancellationToken,
    ) -> OrchestrationHandle {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (updates, first_subscriber) = broadcast::channel(UPDATE_CAPACITY);
        let (state_tx, state_rx) = watch::channel(state.clone());
        let run_id = state.run_id.clone();
        let run = RunLoop {
            runner: self.clone(),
            state,
            last_sequence,
            elapsed_before_ms,
            started: Instant::now(),
            log: Vec::new(),
            updates: updates.clone(),
            state_tx,
            control_rx,
            control_open: true,
            cancellation: cancellation.clone(),
            cancel_applied: false,
            active: None,
            result: None,
        };
        OrchestrationHandle {
            run_id,
            control: control_tx,
            updates,
            first_subscriber: Some(first_subscriber),
            state: state_rx,
            cancellation,
            join: tokio::spawn(run.drive(first)),
        }
    }
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

struct ActiveStep {
    node_id: OrchestrationNodeId,
    attempt: u32,
    token: CancellationToken,
    join: JoinHandle<StepExecution>,
    signals: mpsc::UnboundedReceiver<StepSignal>,
    signals_open: bool,
    permissions: mpsc::UnboundedSender<PermissionResolution>,
    delegated: Option<DelegatedRunRef>,
    usage: UsageSummary,
    started_at_ms: u64,
    /// Last time the attempt showed a sign of life; feeds the stall watchdog.
    last_activity: Instant,
    /// Set when the watchdog cancelled the attempt, so its outcome is
    /// reported as a retryable stall rather than a plain cancellation.
    stalled: bool,
}

struct StepExecution {
    result: Result<Value, OrchestrationError>,
    details: AttemptDetails,
}

enum Wake {
    Cancelled,
    Deadline,
    Stalled,
    Control(Option<Control>),
    Signal(Option<StepSignal>),
    StepDone(Box<Result<StepExecution, tokio::task::JoinError>>),
}

/// Failure while applying a command: a rejected transition is recoverable
/// for user controls, a persistence failure never is.
enum ApplyError {
    Rejected(TransitionError),
    Fatal(OrchestrationRuntimeError),
}

impl From<ApplyError> for OrchestrationRuntimeError {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Rejected(error) => error.into(),
            ApplyError::Fatal(error) => error,
        }
    }
}

struct RunLoop {
    runner: OrchestrationRunner,
    state: OrchestrationRunState,
    last_sequence: u64,
    elapsed_before_ms: u64,
    started: Instant,
    log: Vec<OrchestrationEventEnvelope>,
    updates: broadcast::Sender<OrchestrationUpdate>,
    state_tx: watch::Sender<OrchestrationRunState>,
    control_rx: mpsc::UnboundedReceiver<Control>,
    control_open: bool,
    cancellation: CancellationToken,
    cancel_applied: bool,
    active: Option<ActiveStep>,
    result: Option<OrchestrationResult>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

impl RunLoop {
    async fn drive(
        mut self,
        first: OrchestrationCommand,
    ) -> Result<OrchestrationRunOutput, OrchestrationRuntimeError> {
        let outcome = self.drive_inner(first).await;
        if let Some(active) = self.active.take() {
            active.token.cancel();
        }
        outcome?;
        let result = self
            .result
            .take()
            .expect("drive_inner returns only once finished");
        Ok(OrchestrationRunOutput {
            state: self.state,
            result,
            events: self.log,
        })
    }

    async fn drive_inner(
        &mut self,
        first: OrchestrationCommand,
    ) -> Result<(), OrchestrationRuntimeError> {
        self.apply(first).await?;
        loop {
            if self.result.is_some() {
                return Ok(());
            }
            if self.cancellation.is_cancelled() && !self.cancel_applied {
                self.cancel_applied = true;
                if self.state.status != OrchestrationStatus::Cancelling {
                    self.apply(OrchestrationCommand::Cancel).await?;
                }
                continue;
            }
            if let Some(error) = self.elapsed_exhausted() {
                self.apply(OrchestrationCommand::Abort { error }).await?;
                continue;
            }
            if self.active.is_none() {
                if self.state.status == OrchestrationStatus::Cancelling {
                    self.apply(OrchestrationCommand::CancellationCompleted)
                        .await?;
                    continue;
                }
                if !self.state.paused {
                    if let Some(node_id) = self
                        .state
                        .steps
                        .iter()
                        .find(|(_, step)| step.status == StepStatus::RetryScheduled)
                        .map(|(id, _)| id.clone())
                    {
                        self.apply(OrchestrationCommand::RetryStep { node_id })
                            .await?;
                        continue;
                    }
                    if let Some(node_id) = ready_steps(&self.runner.compiled, &self.state)
                        .into_iter()
                        .next()
                    {
                        let attempt = self.state.steps[&node_id].attempts.len() as u32 + 1;
                        self.apply(OrchestrationCommand::StepAdmitted { node_id, attempt })
                            .await?;
                        continue;
                    }
                    return Err(OrchestrationRuntimeError::Stalled(self.state.status));
                }
            }
            match self.wait().await {
                Wake::Cancelled | Wake::Deadline => {}
                Wake::Stalled => self.on_stalled(),
                Wake::Control(Some(control)) => self.on_control(control).await?,
                Wake::Control(None) => self.control_open = false,
                Wake::Signal(Some(signal)) => self.on_signal(signal).await?,
                Wake::Signal(None) => {
                    if let Some(active) = &mut self.active {
                        active.signals_open = false;
                    }
                }
                Wake::StepDone(joined) => self.on_step_done(*joined).await?,
            }
        }
    }

    async fn wait(&mut self) -> Wake {
        let deadline = self.deadline();
        let stall_deadline = self.stall_deadline();
        let (signals, join) = match self.active.as_mut() {
            Some(active) => (
                active.signals_open.then_some(&mut active.signals),
                Some(&mut active.join),
            ),
            None => (None, None),
        };
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled(), if !self.cancel_applied => Wake::Cancelled,
            control = self.control_rx.recv(), if self.control_open => Wake::Control(control),
            signal = async {
                match signals {
                    Some(signals) => signals.recv().await,
                    None => pending().await,
                }
            } => Wake::Signal(signal),
            joined = async {
                match join {
                    Some(join) => join.await,
                    None => pending().await,
                }
            } => Wake::StepDone(Box::new(joined)),
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => pending().await,
                }
            } => Wake::Deadline,
            _ = async {
                match stall_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => pending().await,
                }
            } => Wake::Stalled,
        }
    }

    /// When the active attempt becomes stale, if the watchdog applies: a
    /// limit is set, a step is running and not already cancelled, and it is
    /// neither paused nor waiting on a person's permission decision.
    fn stall_deadline(&self) -> Option<Instant> {
        let limit = self.runner.compiled.definition.policies.stall_timeout_ms?;
        let active = self.active.as_ref().filter(|active| !active.stalled)?;
        if self.state.paused
            || !self.state.steps[&active.node_id]
                .pending_permissions
                .is_empty()
        {
            return None;
        }
        Some(active.last_activity + Duration::from_millis(limit))
    }

    fn on_stalled(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        tracing::warn!(node = %active.node_id, attempt = active.attempt, "step stalled; cancelling attempt");
        active.stalled = true;
        active.token.cancel();
    }

    fn elapsed_ms(&self) -> u64 {
        self.elapsed_before_ms + self.started.elapsed().as_millis() as u64
    }

    fn deadline(&self) -> Option<Instant> {
        let limit = self.runner.compiled.definition.policies.max_elapsed_ms?;
        let remaining = limit.saturating_sub(self.elapsed_before_ms);
        Some(self.started + Duration::from_millis(remaining))
    }

    fn elapsed_exhausted(&self) -> Option<OrchestrationError> {
        let limit = self.runner.compiled.definition.policies.max_elapsed_ms?;
        let elapsed = self.elapsed_ms();
        (elapsed >= limit && !self.state.status.is_terminal()).then(|| {
            OrchestrationError::new(
                error_codes::BUDGET_EXHAUSTED,
                format!("elapsed budget of {limit}ms exhausted ({elapsed}ms used)"),
            )
        })
    }

    async fn apply(
        &mut self,
        command: OrchestrationCommand,
    ) -> Result<(), OrchestrationRuntimeError> {
        self.try_apply(command).await.map_err(Into::into)
    }

    /// Reduce, commit, publish, then dispatch — in that order.
    async fn try_apply(&mut self, command: OrchestrationCommand) -> Result<(), ApplyError> {
        let effects =
            apply(&self.runner.compiled, &mut self.state, command).map_err(ApplyError::Rejected)?;

        let timestamp_ms = now_ms();
        let mut sequence = self.last_sequence;
        let mut envelopes = Vec::new();
        let mut finish = None;
        let mut execute = None;
        let mut cancels = Vec::new();
        for effect in effects {
            match effect {
                OrchestrationEffect::Emit(event) => {
                    sequence += 1;
                    envelopes.push(OrchestrationEventEnvelope {
                        run_id: self.state.run_id.clone(),
                        definition_id: self.state.definition_id.clone(),
                        revision: self.state.definition_revision,
                        sequence,
                        timestamp_ms,
                        event,
                    });
                }
                OrchestrationEffect::Finish(result) => finish = Some(result),
                OrchestrationEffect::ExecuteStep { node_id, attempt } => {
                    execute = Some((node_id, attempt))
                }
                OrchestrationEffect::CancelStep { node_id, attempt } => {
                    cancels.push((node_id, attempt))
                }
            }
        }

        if let Some(store) = &self.runner.store {
            let snapshot = OrchestrationSnapshot {
                state: self.state.clone(),
                last_sequence: sequence,
                elapsed_ms: self.elapsed_ms(),
                result: finish.clone(),
            };
            store
                .commit(&envelopes, &snapshot)
                .await
                .map_err(|error| ApplyError::Fatal(error.into()))?;
        }
        self.last_sequence = sequence;

        for envelope in envelopes {
            let _ = self
                .updates
                .send(OrchestrationUpdate::Event(envelope.clone()));
            self.log.push(envelope);
        }
        self.state_tx.send_replace(self.state.clone());

        for (node_id, attempt) in cancels {
            if let Some(active) = &self.active {
                if active.node_id == node_id && active.attempt == attempt {
                    active.token.cancel();
                }
            }
        }
        if let Some(result) = finish {
            self.result = Some(result);
        }
        if let Some((node_id, attempt)) = execute {
            self.dispatch(node_id, attempt);
        }
        Ok(())
    }

    fn dispatch(&mut self, node_id: OrchestrationNodeId, attempt: u32) {
        let node = self.runner.compiled.nodes[&node_id].clone();
        let token = self.cancellation.child_token();
        let (signals_tx, signals) = mpsc::unbounded_channel();
        let (permissions, permissions_rx) = mpsc::unbounded_channel();
        let context = StepContext::new(token.clone(), signals_tx, permissions_rx);
        let job = StepJob {
            runner: self.runner.clone(),
            state: self.state.clone(),
            node,
            attempt,
        };
        self.active = Some(ActiveStep {
            node_id,
            attempt,
            token,
            join: tokio::spawn(job.run(context)),
            signals,
            signals_open: true,
            permissions,
            delegated: None,
            usage: UsageSummary::default(),
            started_at_ms: now_ms(),
            last_activity: Instant::now(),
            stalled: false,
        });
    }

    fn correlation(&self, active: &ActiveStep) -> StepCorrelation {
        StepCorrelation {
            run_id: self.state.run_id.clone(),
            definition_id: self.state.definition_id.clone(),
            revision: self.state.definition_revision,
            node_id: active.node_id.clone(),
            attempt: active.attempt,
            delegated: active.delegated.clone().unwrap_or_default(),
        }
    }

    async fn on_signal(&mut self, signal: StepSignal) -> Result<(), OrchestrationRuntimeError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        active.last_activity = Instant::now();
        match signal {
            StepSignal::Checkpoint { value, committed } => {
                let node_id = active.node_id.clone();
                let attempt = active.attempt;
                let result = self
                    .apply(OrchestrationCommand::RecordCheckpoint {
                        node_id,
                        attempt,
                        value,
                    })
                    .await;
                let _ = committed.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
                result?;
            }
            StepSignal::Delegated(delegated) => active.delegated = Some(delegated),
            StepSignal::Usage(usage) => {
                active.usage = usage;
                let mut total = self.state.usage.clone();
                total.add(&active.usage);
                let policies = &self.runner.compiled.definition.policies;
                let exhausted = [
                    (
                        "model request",
                        policies.max_model_requests,
                        total.model_requests,
                    ),
                    ("tool call", policies.max_tool_calls, total.tool_calls),
                    ("token", policies.max_tokens, total.tokens),
                ]
                .into_iter()
                .find(|(_, limit, used)| limit.is_some_and(|limit| *used >= limit));
                let message = exhausted
                    .map(|(name, limit, used)| {
                        format!(
                            "{name} budget of {} exhausted ({used} used)",
                            limit.unwrap()
                        )
                    })
                    .or_else(|| {
                        policies
                            .max_cost_usd
                            .filter(|limit| total.cost_usd >= *limit)
                            .map(|limit| format!("cost budget of ${limit} exhausted"))
                    });
                if let Some(message) = message {
                    self.apply(OrchestrationCommand::Abort {
                        error: OrchestrationError::new(error_codes::BUDGET_EXHAUSTED, message),
                    })
                    .await?;
                }
            }
            StepSignal::Agent(envelope) => {
                let correlation = self.correlation(self.active.as_ref().expect("active"));
                let _ =
                    self.updates
                        .send(OrchestrationUpdate::Agent(Box::new(CorrelatedAgentEvent {
                            correlation,
                            envelope: *envelope,
                        })));
            }
            StepSignal::PermissionRequested { permission_id, .. } => {
                let command = OrchestrationCommand::PermissionRequired {
                    node_id: active.node_id.clone(),
                    attempt: active.attempt,
                    permission_id,
                };
                match self.try_apply(command).await {
                    Ok(()) => {}
                    Err(ApplyError::Rejected(error)) => {
                        tracing::warn!(code = error.code, %error, "ignored permission request")
                    }
                    Err(ApplyError::Fatal(error)) => return Err(error),
                }
            }
        }
        Ok(())
    }

    async fn on_control(&mut self, control: Control) -> Result<(), OrchestrationRuntimeError> {
        // Pausing, resuming and answering a permission all restart the stall
        // clock, which does not run while waiting on a person.
        if let Some(active) = self.active.as_mut() {
            active.last_activity = Instant::now();
        }
        let (command, reply) = match control {
            Control::Pause(reply) => (Ok(OrchestrationCommand::Pause), reply),
            Control::Resume(reply) => (Ok(OrchestrationCommand::Resume), reply),
            Control::ResolvePermission {
                permission_id,
                decision,
                reply,
            } => {
                let command = match &self.active {
                    Some(active)
                        if self.state.steps[&active.node_id]
                            .pending_permissions
                            .contains(&permission_id) =>
                    {
                        // Deliver to the delegated agent first: if the attempt
                        // already ended, the decision has nowhere to go.
                        let approved = matches!(decision, PermissionDecision::Approved);
                        match active.permissions.send(PermissionResolution {
                            permission_id: permission_id.clone(),
                            decision,
                        }) {
                            Ok(()) => Ok(OrchestrationCommand::PermissionResolved {
                                node_id: active.node_id.clone(),
                                attempt: active.attempt,
                                permission_id,
                                approved,
                            }),
                            Err(_) => Err(ControlError::Rejected(
                                "the attempt awaiting this permission has ended".into(),
                            )),
                        }
                    }
                    _ => Err(ControlError::Rejected(format!(
                        "no active step is waiting on permission {permission_id}"
                    ))),
                };
                (command, reply)
            }
        };
        let outcome = match command {
            Err(error) => Err(error),
            Ok(command) => match self.try_apply(command).await {
                Ok(()) => Ok(()),
                Err(ApplyError::Rejected(error)) => Err(ControlError::Rejected(error.message)),
                Err(ApplyError::Fatal(error)) => {
                    let _ = reply.send(Err(ControlError::Rejected(error.to_string())));
                    return Err(error);
                }
            },
        };
        let _ = reply.send(outcome);
        Ok(())
    }

    async fn on_step_done(
        &mut self,
        joined: Result<StepExecution, tokio::task::JoinError>,
    ) -> Result<(), OrchestrationRuntimeError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        // Observations sent just before the executor returned.
        while let Ok(signal) = active.signals.try_recv() {
            match signal {
                StepSignal::Delegated(delegated) => active.delegated = Some(delegated),
                StepSignal::Usage(usage) => {
                    active.usage = usage;
                    let mut total = self.state.usage.clone();
                    total.add(&active.usage);
                    let policies = &self.runner.compiled.definition.policies;
                    let exhausted = [
                        (
                            "model request",
                            policies.max_model_requests,
                            total.model_requests,
                        ),
                        ("tool call", policies.max_tool_calls, total.tool_calls),
                        ("token", policies.max_tokens, total.tokens),
                    ]
                    .into_iter()
                    .find(|(_, limit, used)| limit.is_some_and(|limit| *used >= limit));
                    let message = exhausted
                        .map(|(name, limit, used)| {
                            format!(
                                "{name} budget of {} exhausted ({used} used)",
                                limit.unwrap()
                            )
                        })
                        .or_else(|| {
                            policies
                                .max_cost_usd
                                .filter(|limit| total.cost_usd >= *limit)
                                .map(|limit| format!("cost budget of ${limit} exhausted"))
                        });
                    if let Some(message) = message {
                        self.apply(OrchestrationCommand::Abort {
                            error: OrchestrationError::new(error_codes::BUDGET_EXHAUSTED, message),
                        })
                        .await?;
                    }
                }
                StepSignal::Checkpoint { committed, .. } => {
                    let _ = committed.send(Err("attempt ended before checkpoint commit".into()));
                }
                StepSignal::Agent(_) | StepSignal::PermissionRequested { .. } => {}
            }
        }
        // Cancelled or aborted while running: the reducer already recorded
        // the attempt as cancelled.
        if self.state.status.is_terminal() || self.state.status == OrchestrationStatus::Cancelling {
            return Ok(());
        }
        let execution = joined.unwrap_or_else(|error| StepExecution {
            result: Err(OrchestrationError::new(
                "step_panicked",
                format!("step executor terminated abnormally: {error}"),
            )),
            details: AttemptDetails::default(),
        });
        let mut execution = execution;
        if active.stalled {
            execution.result = Err(OrchestrationError::retryable(
                "step_stalled",
                format!(
                    "step {} made no progress for {}ms and was cancelled",
                    active.node_id,
                    self.runner
                        .compiled
                        .definition
                        .policies
                        .stall_timeout_ms
                        .unwrap_or_default()
                ),
                RetryReason::BackendTimeout,
            ));
        }
        let mut details = execution.details;
        details.started_at_ms = Some(active.started_at_ms);
        details.ended_at_ms = Some(now_ms());
        details.delegated = active.delegated;
        details.usage = active.usage;
        let command = match execution.result {
            Ok(output) => OrchestrationCommand::StepSucceeded {
                node_id: active.node_id,
                attempt: active.attempt,
                output,
                details,
            },
            Err(error) => OrchestrationCommand::StepFailed {
                node_id: active.node_id,
                attempt: active.attempt,
                error,
                details,
            },
        };
        self.apply(command).await
    }
}

// ---------------------------------------------------------------------------
// Step job
// ---------------------------------------------------------------------------

/// One attempt, executed on its own task against a snapshot of the state
/// taken at admission (steps run one at a time, so it cannot go stale).
struct StepJob {
    runner: OrchestrationRunner,
    state: OrchestrationRunState,
    node: OrchestrationNode,
    attempt: u32,
}

impl StepJob {
    async fn run(self, context: StepContext) -> StepExecution {
        let cancellation = context.cancellation.clone();
        let mut details = AttemptDetails::default();
        let work = self.execute(context, &mut details);
        // Executors observe the attempt token themselves so they can cancel
        // delegated work cleanly; the grace period bounds a misbehaving one.
        let result = tokio::select! {
            _ = async {
                cancellation.cancelled().await;
                tokio::time::sleep(CANCEL_GRACE).await;
            } => Err(OrchestrationError::new("cancelled", "step cancelled")),
            result = async {
                match self.node.timeout_ms {
                    Some(limit) => tokio::time::timeout(Duration::from_millis(limit), work)
                        .await
                        .unwrap_or_else(|_| Err(OrchestrationError::retryable(
                            "step_timeout",
                            format!("step {} timed out after {limit}ms", self.node.id),
                            RetryReason::BackendTimeout,
                        ))),
                    None => work.await,
                }
            } => result,
        };
        StepExecution { result, details }
    }

    async fn execute(
        &self,
        context: StepContext,
        details: &mut AttemptDetails,
    ) -> Result<Value, OrchestrationError> {
        let runner = &self.runner;
        let validation = Validation {
            schemas: runner.schemas.as_ref(),
            validator: runner.validator.as_ref(),
        };
        let definition = &runner.compiled.definition;
        match &self.node.kind {
            OrchestrationNodeKind::Input(config) => {
                let input = execute_input(
                    &self.state,
                    config,
                    definition.input_schema.as_ref(),
                    &validation,
                )?;
                details.input = self.state.input.clone();
                Ok(input)
            }
            OrchestrationNodeKind::Agent(config) => {
                let input = resolve_inputs(&self.state, &self.node.input_bindings)?;
                details.input = Some(input.clone());
                let schema = if config.structured_output
                    == harness_core::orchestration::StructuredOutputMode::Text
                {
                    serde_json::json!({ "type": "string" })
                } else {
                    let schema_ref = self.node.output_schema.as_ref().ok_or_else(|| {
                        OrchestrationError::new(
                            "missing_output_schema",
                            "agent node has no output schema",
                        )
                    })?;
                    validation.resolve(schema_ref)?
                };
                let tools = match &config.tools {
                    ToolScope::None => Vec::new(),
                    ToolScope::AllowList(tools) => tools.clone(),
                    ToolScope::Inherit => runner
                        .available_tools
                        .as_deref()
                        .cloned()
                        .ok_or_else(|| {
                            OrchestrationError::new(
                                "tool_scope_unresolved",
                                "tool scope `inherit` needs the host's tool list (with_available_tools)",
                            )
                        })?,
                };
                details.tools = Some(tools.clone());
                let request = AgentStepRequest {
                    task_queue: config.task_queue.clone(),
                    checkpoint: self.state.steps[&self.node.id].checkpoint.clone(),
                    run_id: self.state.run_id.clone(),
                    node_id: self.node.id.clone(),
                    attempt: self.attempt,
                    instructions: config.instructions.clone(),
                    input,
                    output_schema: schema.clone(),
                    tools,
                    model: config.model.clone(),
                    structured_output: config.structured_output,
                    context_mode: config.context_mode,
                    profile: config.profile.clone(),
                    feedback: self.state.steps[&self.node.id].feedback.clone(),
                };
                let output = runner.agent.execute(request, context).await?;
                validation.check(
                    &schema,
                    &output.value,
                    "invalid_structured_output",
                    Some(RetryReason::InvalidStructuredOutput),
                )?;
                Ok(output.value)
            }
            OrchestrationNodeKind::Verify(config) => {
                let compiled = &runner.compiled;
                let producers = |node_id: &OrchestrationNodeId| {
                    compiled
                        .node(node_id)
                        .and_then(|node| node.output_schema.as_ref())
                        .and_then(|reference| validation.resolve(reference).ok())
                };
                let (result, evidence) = execute_verify(
                    &self.state,
                    &self.node,
                    config,
                    &producers,
                    &validation,
                    runner.artifacts.as_ref(),
                )
                .await;
                details.evidence = evidence;
                result
            }
            OrchestrationNodeKind::Output(config) => {
                let value = resolve_binding(&self.state, &config.source)?;
                // The definition's output contract is the final public
                // contract; `strict = false` on both skips validation.
                if config.strict || definition.output_contract.strict {
                    let schema = validation.resolve(&definition.output_contract.schema)?;
                    validation.check(&schema, &value, "invalid_final_output", None)?;
                }
                Ok(value)
            }
        }
    }
}

//! The task that owns one run: decides the next command, waits for what
//! happens next, and hands each wake-up to its handler.
//!
//! Handlers live in submodules: `commit` applies a command (reduce, persist,
//! publish, dispatch), `control` answers caller requests, `signals` follows the
//! active attempt, and `clock` keeps elapsed time without the user's waits.

mod clock;
mod commit;
mod control;
mod signals;

use std::{future::pending, time::Duration};

use harness_core::orchestration::{
    ready_steps, DelegatedRunRef, OrchestrationCommand, OrchestrationError, OrchestrationNodeId,
    OrchestrationResult, OrchestrationRunState, OrchestrationStatus, StepStatus, TransitionError,
    UsageSummary,
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::super::{
    journal,
    steps::{InputAnswer, PermissionResolution, StepContext, StepSignal},
    store::OrchestrationEventEnvelope,
};
use super::{
    handle::Control,
    now_ms,
    step_job::{StepExecution, StepJob},
    OrchestrationHandle, OrchestrationRunOutput, OrchestrationRunner, OrchestrationRuntimeError,
    OrchestrationUpdate, StepCorrelation,
};
use clock::RunClock;

const UPDATE_CAPACITY: usize = 1024;

/// Starts the task that drives a run and returns its handle.
pub(super) fn spawn(
    runner: OrchestrationRunner,
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
        runner,
        state,
        last_sequence,
        clock: RunClock::new(elapsed_before_ms),
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
    OrchestrationHandle::new(
        run_id,
        control_tx,
        updates,
        first_subscriber,
        state_rx,
        cancellation,
        tokio::spawn(run.drive(first)),
    )
}

struct ActiveStep {
    node_id: OrchestrationNodeId,
    attempt: u32,
    token: CancellationToken,
    join: JoinHandle<StepExecution>,
    signals: mpsc::UnboundedReceiver<StepSignal>,
    signals_open: bool,
    permissions: mpsc::UnboundedSender<PermissionResolution>,
    answers: mpsc::UnboundedSender<InputAnswer>,
    delegated: Option<DelegatedRunRef>,
    usage: UsageSummary,
    started_at_ms: u64,
    /// Last time the attempt showed a sign of life; feeds the stall watchdog.
    last_activity: Instant,
    /// Set when the watchdog cancelled the attempt, so its outcome is
    /// reported as a retryable stall rather than a plain cancellation.
    stalled: bool,
    /// What the attempt's tool calls did.
    actions: journal::ActionRecorder,
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
    clock: RunClock,
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
        while self.result.is_none() {
            match self.next_command()? {
                Some(command) => self.apply(command).await?,
                None => {
                    let wake = self.wait().await;
                    self.on_wake(wake).await?;
                }
            }
        }
        Ok(())
    }

    /// The command the run needs next without waiting on anything, or `None`
    /// when it has to wait (a step is running, or the run is paused).
    fn next_command(&mut self) -> Result<Option<OrchestrationCommand>, OrchestrationRuntimeError> {
        if self.cancellation.is_cancelled() && !self.cancel_applied {
            self.cancel_applied = true;
            if self.state.status != OrchestrationStatus::Cancelling {
                return Ok(Some(OrchestrationCommand::Cancel));
            }
        }
        if let Some(error) = self.elapsed_exhausted() {
            return Ok(Some(OrchestrationCommand::Abort { error }));
        }
        if self.active.is_some() {
            return Ok(None);
        }
        if self.state.status == OrchestrationStatus::Cancelling {
            return Ok(Some(OrchestrationCommand::CancellationCompleted));
        }
        if self.state.paused {
            return Ok(None);
        }
        self.admission()
            .map(Some)
            .ok_or(OrchestrationRuntimeError::Stalled(self.state.status))
    }

    /// A scheduled retry first, otherwise the first ready step.
    fn admission(&self) -> Option<OrchestrationCommand> {
        let retry = self
            .state
            .steps
            .iter()
            .find(|(_, step)| step.status == StepStatus::RetryScheduled)
            .map(|(node_id, _)| node_id.clone());
        if let Some(node_id) = retry {
            return Some(OrchestrationCommand::RetryStep { node_id });
        }
        let node_id = ready_steps(&self.runner.compiled, &self.state)
            .into_iter()
            .next()?;
        let attempt = self.state.steps[&node_id].attempts.len() as u32 + 1;
        Some(OrchestrationCommand::StepAdmitted { node_id, attempt })
    }

    async fn on_wake(&mut self, wake: Wake) -> Result<(), OrchestrationRuntimeError> {
        match wake {
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
        Ok(())
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
            _ = sleep_until(deadline) => Wake::Deadline,
            _ = sleep_until(stall_deadline) => Wake::Stalled,
        }
    }

    /// When the active attempt becomes stale, if the watchdog applies: a
    /// limit is set, a step is running and not already cancelled, and it is
    /// neither paused nor waiting on a person's permission decision or answer.
    fn stall_deadline(&self) -> Option<Instant> {
        let limit = self.runner.compiled.definition.policies.stall_timeout_ms?;
        let active = self.active.as_ref().filter(|active| !active.stalled)?;
        let step = &self.state.steps[&active.node_id];
        if self.state.paused || !step.pending_permissions.is_empty() || step.pending_input.is_some()
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

    fn deadline(&self) -> Option<Instant> {
        self.clock
            .deadline(self.runner.compiled.definition.policies.max_elapsed_ms)
    }

    fn elapsed_exhausted(&self) -> Option<OrchestrationError> {
        self.clock.exhausted(
            self.runner.compiled.definition.policies.max_elapsed_ms,
            self.state.status.is_terminal(),
        )
    }

    fn dispatch(&mut self, node_id: OrchestrationNodeId, attempt: u32) {
        let node = self.runner.compiled.nodes[&node_id].clone();
        let token = self.cancellation.child_token();
        let (signals_tx, signals) = mpsc::unbounded_channel();
        let (permissions, permissions_rx) = mpsc::unbounded_channel();
        let (answers, answers_rx) = mpsc::unbounded_channel();
        let context = StepContext::new(token.clone(), signals_tx, permissions_rx, answers_rx);
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
            answers,
            delegated: None,
            usage: UsageSummary::default(),
            started_at_ms: now_ms(),
            last_activity: Instant::now(),
            stalled: false,
            actions: Default::default(),
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

    /// Sends to subscribers. A broadcast send fails only when nobody is
    /// subscribed, which is normal: callers that just await the result never
    /// subscribe, and the log keeps every committed event regardless.
    fn publish(&self, update: OrchestrationUpdate) {
        let _ = self.updates.send(update);
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}

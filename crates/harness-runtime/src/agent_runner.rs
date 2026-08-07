//! Async runtime loop for a single deterministic agent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::transcript::validate_transcript;
use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::{AgentCommand, AgentError, AgentResult, AgentStatus, UserInput};
use harness_protocol::effects::{AgentEffect, SessionMutation, SpawnAgentSpec, SpawnMode, ToolRequest};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp, ToolCallId};
use harness_session_store::SessionCommitter;

use crate::agent_supervisor::{AgentSupervisor, SupervisorError};
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::session_runtime::{stored_agent_state, AgentProjectionTable, LiveStateTable};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

/// Mailbox and control handles for one agent.
pub struct AgentTask {
    pub id: AgentId,
    pub commands: mpsc::Receiver<AgentCommand>,
    /// Extra sender kept so background tasks can enqueue commands.
    pub commands_tx: mpsc::Sender<AgentCommand>,
    pub events: broadcast::Sender<AgentEventEnvelope>,
    pub cancel: CancellationToken,
}

impl AgentTask {
    /// Creates a mailbox with default channel capacities.
    pub fn new(id: AgentId) -> (Self, mpsc::Sender<AgentCommand>) {
        Self::new_with_capacities(id, 64, 256)
    }

    /// Creates a mailbox with caller-selected channel capacities.
    pub fn new_with_capacities(
        id: AgentId,
        command_capacity: usize,
        event_capacity: usize,
    ) -> (Self, mpsc::Sender<AgentCommand>) {
        let (commands_tx, commands) = mpsc::channel(command_capacity);
        let (events, _) = broadcast::channel(event_capacity);
        let task = Self {
            id,
            commands,
            commands_tx: commands_tx.clone(),
            events,
            cancel: CancellationToken::new(),
        };
        (task, commands_tx)
    }
}

/// Drives an agent's state machine and dispatches its effects asynchronously.
pub struct AgentRunner {
    pub agent: Agent,
    pub task: AgentTask,
    pub backend: Arc<dyn ExecutionBackend>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub workspace: Arc<dyn Workspace>,
    pub event_sink: Arc<dyn EventSink>,
    pub agent_sequence: u64,
    pub cancel: CancellationToken,
    pub scheduler: Arc<Scheduler>,
    /// Shared per-agent live state published after every transition.
    live_state: LiveStateTable,
    /// Shared per-agent durable projection (RC-302), published after every
    /// transition so a checkpoint always reflects truthful agent state.
    projection: Option<AgentProjectionTable>,
    /// The session's authoritative commit boundary (RC-301). When set, every
    /// emitted event is committed (sequence allocation + durable persistence
    /// with the final sequence, and truthful publish-on-success semantics)
    /// before publication; the session bus then preserves the assigned
    /// sequence so stored and observed order agree.
    committer: Option<Arc<SessionCommitter>>,
    backend_tokens: HashMap<RunId, CancellationToken>,
    tool_tokens: HashMap<ToolCallId, CancellationToken>,
    /// Outcome of the most recently completed `FinishRun` effect, retrieved
    /// via [`Self::take_final_result`].
    ///
    /// This is overwritten on every completed run rather than queued: it
    /// reflects the *latest* run's outcome. Callers that need durable
    /// per-run history should read it from the transcript/event stream
    /// instead of relying on this field surviving multiple runs.
    final_result: Option<Result<AgentResult, AgentError>>,
    supervision: Option<AgentSupervision>,
    /// Optional durable store used by `Persist` effects.
    session_store: Option<Arc<dyn harness_session_store::SessionStore>>,
    /// A session-integrity failure that must terminate even a long-lived
    /// root runner. Run-level provider/tool failures remain recoverable, but
    /// losing the strict durable commit boundary does not.
    fatal_error: bool,
    /// When `true`, the mailbox loop in [`Self::run`] survives the
    /// completion of an individual run (`AgentEffect::FinishRun`, whether
    /// the outcome was success, failure, or the run-scoped cancel path) and
    /// keeps accepting further `AgentCommand`s until the mailbox itself is
    /// closed or the runner's own [`CancellationToken`] fires.
    ///
    /// Defaults to `false`, which preserves the historical one-shot
    /// behavior (the loop exits as soon as any run finishes). Root session
    /// agents should opt in via [`Self::long_lived`] so a session supports
    /// multiple sequential prompts without recreating the root agent
    /// (`upgrade_rusty.md` RST-002). Child agents should generally leave
    /// this `false` so an awaited child still terminates promptly after its
    /// single assigned task completes.
    long_lived: bool,
}

/// Session-scoped services required to interpret child-agent effects.
#[derive(Clone)]
pub struct AgentSupervision {
    supervisor: Arc<dyn SupervisorControl>,
    pub integrations: Arc<IntegrationRegistry>,
}

#[async_trait]
trait SupervisorControl: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn spawn_child(
        &self,
        parent: &Agent,
        parent_backend: Arc<dyn ExecutionBackend>,
        integration_registry: &IntegrationRegistry,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        scheduler: Arc<Scheduler>,
        parent_commands_tx: mpsc::Sender<AgentCommand>,
        spec: SpawnAgentSpec,
    ) -> Result<AgentId, SupervisorError>;

    async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError>;
    async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError>;
    /// M5: fetches the command channel for a just-spawned child so its
    /// initial task (`SpawnAgentSpec::task`) can be delivered as a
    /// `StartRun`. `None` if the child isn't registered (already finished,
    /// or the id is stale).
    async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>>;
}

/// Outcome of a spawn attempt, returned so callers other than the plain
/// `AgentEffect::SpawnAgent` path (namely the `agent.spawn` tool — see
/// `AgentRunner::execute_tool`) can build a meaningful result of their own
/// without duplicating any of the supervisor-enforced spawn logic.
///
/// `awaited` is best-effort observability only: the *authoritative*
/// state-machine update always happens independently, via the child's own
/// runner sending `AgentCommand::ChildCompleted`/`ChildFailed` directly to
/// this agent's mailbox when it finishes (see
/// `AgentSupervisor::spawn_child`) — this field exists only so a
/// tool-triggered `AwaitResult` spawn can report what happened back to the
/// calling model in its `ToolResult`, not to drive any core state.
///
/// Distinct from `agent_supervisor::SpawnOutcome` (which distinguishes
/// `Detached`/`Awaited` for `spawn_and_drive`'s own callers) — this type is
/// specifically the `agent_runner`-level view used by the tool-interception
/// path in `execute_tool`.
pub(crate) struct ToolSpawnOutcome {
    pub child_id: AgentId,
    pub awaited: Option<Result<AgentResult, String>>,
}

#[async_trait]
impl SupervisorControl for AgentSupervisor {
    async fn spawn_child(
        &self,
        parent: &Agent,
        parent_backend: Arc<dyn ExecutionBackend>,
        integration_registry: &IntegrationRegistry,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        scheduler: Arc<Scheduler>,
        parent_commands_tx: mpsc::Sender<AgentCommand>,
        spec: SpawnAgentSpec,
    ) -> Result<AgentId, SupervisorError> {
        AgentSupervisor::spawn_child(
            self,
            parent,
            parent_backend,
            integration_registry,
            tool_registry,
            workspace,
            event_sink,
            scheduler,
            parent_commands_tx,
            spec,
        )
        .await
    }

    async fn await_child(&self, child_id: AgentId) -> Result<AgentResult, SupervisorError> {
        AgentSupervisor::await_child(self, child_id).await
    }

    async fn cancel_child(&self, child_id: AgentId) -> Result<(), SupervisorError> {
        AgentSupervisor::cancel_child(self, child_id).await
    }

    async fn child_commands(&self, child_id: AgentId) -> Option<mpsc::Sender<AgentCommand>> {
        AgentSupervisor::child_commands(self, child_id).await
    }
}

impl AgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: Agent,
        mut task: AgentTask,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        cancel: CancellationToken,
        live_state: LiveStateTable,
        scheduler: Arc<Scheduler>,
    ) -> Self {
        task.cancel = cancel.clone();
        Self {
            agent,
            task,
            backend,
            tool_registry,
            workspace,
            event_sink,
            agent_sequence: 0,
            cancel,
            scheduler,
            live_state,
            projection: None,
            committer: None,
            backend_tokens: HashMap::new(),
            tool_tokens: HashMap::new(),
            final_result: None,
            supervision: None,
            session_store: None,
            fatal_error: false,
            long_lived: false,
        }
    }

    /// Enables production interpretation of `SpawnAgent` and `CancelChild`.
    pub fn with_supervision(
        mut self,
        supervisor: AgentSupervisor,
        integrations: Arc<IntegrationRegistry>,
    ) -> Self {
        self.supervision = Some(AgentSupervision {
            supervisor: Arc::new(supervisor),
            integrations,
        });
        self
    }

    /// Enables durable persistence of `Persist` effects through `store`.
    pub fn with_session_store(
        mut self,
        store: Arc<dyn harness_session_store::SessionStore>,
    ) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Routes emitted events through the session's authoritative committer
    /// (RC-301): sequence allocation, durable persistence with the final
    /// sequence, and truthful publish-on-success semantics.
    pub fn with_committer(mut self, committer: Arc<SessionCommitter>) -> Self {
        self.committer = Some(committer);
        self
    }

    /// Publishes this runner's durable agent projection after every
    /// transition (RC-302), so session checkpoints are always truthful.
    pub fn with_projection(mut self, projection: AgentProjectionTable) -> Self {
        self.projection = Some(projection);
        self
    }

    /// Opts this runner into surviving individual run completion.
    ///
    /// Call this for a session's root agent so [`Self::run`] keeps
    /// processing commands (accepting further prompts, steering, and
    /// follow-ups) after a run finishes, instead of exiting its mailbox
    /// loop. Leave this unset (the default) for child agents so an awaited
    /// child still terminates promptly once its single assigned task
    /// completes. See `upgrade_rusty.md` RST-002.
    pub fn long_lived(mut self, long_lived: bool) -> Self {
        self.long_lived = long_lived;
        self
    }

    /// Takes the captured outcome of the most recently completed run's
    /// `FinishRun` effect, if any.
    ///
    /// For a [`Self::long_lived`] runner this may be called after each run
    /// completes; it always reflects the latest run only.
    pub fn take_final_result(&mut self) -> Option<Result<AgentResult, AgentError>> {
        self.final_result.take()
    }

    /// Processes commands until completion, cancellation, or mailbox closure.
    ///
    /// A non-[`Self::long_lived`] runner (the default; used for child
    /// agents and any caller that has not opted in) exits as soon as the
    /// active run finishes, preserving the original one-shot behavior. A
    /// [`Self::long_lived`] runner (opt in for a session's root agent) keeps
    /// looping after a run finishes, so the same mailbox can accept
    /// additional prompts, steering, and follow-up commands without
    /// recreating the agent. In both cases session/runner-level
    /// cancellation (this runner's own [`CancellationToken`]) and mailbox
    /// closure still terminate the loop.
    pub async fn run(&mut self) {
        self.publish_status();
        loop {
            if self.task.commands_tx.strong_count() == 1 {
                break;
            }
            if self.cancel.is_cancelled() {
                self.cancel_run().await;
                break;
            }

            let command = tokio::select! {
                command = self.task.commands.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
                _ = self.cancel.cancelled() => {
                    self.cancel_run().await;
                    break;
                },
                _ = tokio::time::sleep(Duration::from_millis(250)) => continue,
            };

            if self.cancel.is_cancelled() {
                self.cancel_run().await;
                break;
            }

            let effects = self.apply_and_publish(command);
            let finished_run = self.dispatch_effects(effects).await;
            if self.fatal_error {
                break;
            }
            if finished_run && !self.long_lived {
                break;
            }
            if finished_run && self.long_lived {
                let effects = self.apply_and_publish(AgentCommand::StartNextQueuedRun);
                self.dispatch_effects(effects).await;
            }
        }
    }

    /// Applies `Cancel` unless the agent already reached a terminal state.
    async fn cancel_run(&mut self) {
        if !self.is_terminal() {
            let effects = self.apply_and_publish(AgentCommand::Cancel);
            self.dispatch_effects(effects).await;
        }
    }

    fn apply_and_publish(&mut self, command: AgentCommand) -> Vec<AgentEffect> {
        let effects = self.agent.apply(command);
        self.publish_status();
        effects
    }

    fn publish_status(&self) {
        // M4: `self_metrics()` correctly sums cost with "unknown poisons the
        // total" semantics (matching `UsageValue`'s existing token-count
        // discipline) — previously this call site duplicated a *buggy*
        // version of the same calculation inline, one that silently skipped
        // unknown-cost records instead of reporting the total as unknown.
        let child_usage = self
            .agent
            .usage
            .child_usage
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let self_metrics = self.agent.usage.self_metrics();
        let usage =
            harness_core::usage::compute_agent_usage_summary(self_metrics.clone(), &child_usage);

        {
            let mut table = self.live_state.lock().expect("live_state mutex poisoned");
            let entry = table.entry(self.agent.id).or_default();
            entry.status = self.agent.state.status;
            entry.current_operation = self.agent.state.current_operation.clone();
            entry.last_error = self.agent.state.last_error.clone();
            entry.total_requests = self_metrics.total_requests;
            entry.total_cost_usd = self_metrics.total_cost;
            entry.usage = usage;
        }

        // RC-302: publish the durable projection so checkpoints are truthful.
        if let Some(projection) = &self.projection {
            projection
                .lock()
                .expect("projection mutex poisoned")
                .insert(self.agent.id, stored_agent_state(&self.agent));
        }
    }

    fn publish_outcome(&self, outcome: AgentOutcome) {
        let mut table = self.live_state.lock().expect("live_state mutex poisoned");
        let entry = table.entry(self.agent.id).or_default();
        entry.last_outcome = Some(outcome);
    }

    async fn dispatch_effects(&mut self, effects: Vec<AgentEffect>) -> bool {
        let mut finish = false;
        for effect in effects {
            match effect {
                AgentEffect::ExecuteBackend { request } => self.execute_backend(request).await,
                AgentEffect::ExecuteTool { request } => self.execute_tool(request).await,
                AgentEffect::Emit { event } => {
                    if !self.emit(event).await {
                        finish = true;
                        break;
                    }
                }
                AgentEffect::FinishRun { result } => {
                    finish = true;
                    self.final_result = Some(match self.agent.state.status {
                        AgentStatus::Failed => Err(self
                            .agent
                            .state
                            .last_error
                            .clone()
                            .unwrap_or_else(|| AgentError {
                                message: result.summary.clone(),
                                code: "FAILED".into(),
                                details: None,
                            })),
                        _ => Ok(result),
                    });
                }
                AgentEffect::CancelBackend { run_id } => self.cancel_backend(run_id),
                AgentEffect::CancelTool { call_id } => self.cancel_tool(call_id),
                AgentEffect::CancelChild { agent_id } => {
                    if let Some(supervision) = &self.supervision {
                        if let Err(error) = supervision.supervisor.cancel_child(agent_id).await {
                            tracing::warn!(?agent_id, %error, "failed to cancel child agent");
                        }
                    } else {
                        tracing::warn!(?agent_id, "CancelChild requested without a supervisor");
                    }
                }
                AgentEffect::SpawnAgent { spec } => {
                    let _ = self.spawn_agent(spec).await;
                }
                AgentEffect::RequestPermission { request } => {
                    // The preceding PermissionRequested event is the single
                    // host-facing delivery path. This effect deliberately
                    // performs no second publication: the runner remains in
                    // WaitingForPermission until the matching
                    // PermissionResolved command arrives.
                    debug_assert!(self.agent.state.pending_permissions.contains_key(&request.id));
                }
                AgentEffect::Persist { mutation } => {
                    self.persist(mutation).await;
                }
            }
        }
        finish
    }

    async fn spawn_agent(&mut self, spec: SpawnAgentSpec) -> Result<ToolSpawnOutcome, String> {
        let Some(supervision) = self.supervision.clone() else {
            let message = "SpawnAgent requested without a supervisor".to_string();
            tracing::warn!(?spec, "{message}");
            return Err(message);
        };
        let mode = spec.mode;
        let task = spec.task.clone();
        match supervision
            .supervisor
            .spawn_child(
                &self.agent,
                self.backend.clone(),
                supervision.integrations.as_ref(),
                self.tool_registry.clone(),
                self.workspace.clone(),
                self.event_sink.clone(),
                self.scheduler.clone(),
                self.task.commands_tx.clone(),
                spec,
            )
            .await
        {
            Ok(child_id) => {
                // M5: kick off the child's initial task, if one was given —
                // before anything might block on `await_child` below, so an
                // `AwaitResult` spawn doesn't wait on a child that was never
                // told to do anything. Pre-M5 callers that leave `task: None`
                // keep the original behavior: an idle child, started by
                // whatever Rust orchestration code sends it a command later.
                if let Some(task_text) = task {
                    match supervision.supervisor.child_commands(child_id).await {
                        Some(child_tx) => {
                            let _ = child_tx
                                .send(AgentCommand::StartRun {
                                    input: UserInput {
                                        text: task_text,
                                        attachments: vec![],
                                    },
                                })
                                .await;
                        }
                        None => {
                            tracing::warn!(
                                ?child_id,
                                "spawned child has no command channel; its task was not started"
                            );
                        }
                    }
                }

                let awaiting = matches!(mode, SpawnMode::AwaitResult);
                let effects = self.apply_and_publish(AgentCommand::ChildSpawned {
                    agent_id: child_id,
                    awaiting,
                });
                Box::pin(self.dispatch_effects(effects)).await;
                let mut awaited = None;
                if awaiting {
                    match supervision.supervisor.await_child(child_id).await {
                        Ok(result) => awaited = Some(Ok(result)),
                        Err(error) => {
                            let effects = self.apply_and_publish(AgentCommand::ChildFailed {
                                agent_id: child_id,
                                error: AgentError {
                                    message: error.to_string(),
                                    code: "CHILD_AWAIT_FAILED".into(),
                                    details: None,
                                },
                            });
                            Box::pin(self.dispatch_effects(effects)).await;
                            awaited = Some(Err(error.to_string()));
                        }
                    }
                }
                Ok(ToolSpawnOutcome { child_id, awaited })
            }
            Err(error) => {
                tracing::warn!(%error, "failed to spawn child agent");
                Err(error.to_string())
            }
        }
    }

    async fn emit(&mut self, event: AgentEvent) -> bool {
        match &event {
            AgentEvent::Completed { outcome } => self.publish_outcome(*outcome),
            AgentEvent::Failed { .. } => self.publish_outcome(AgentOutcome::Failed),
            _ => {}
        }

        let envelope = AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: self.agent.session_id,
            agent_id: self.agent.id,
            parent_agent_id: self.agent.parent_id,
            run_id: self.agent.state.active_run,
            agent_sequence: self.agent_sequence,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        };
        self.agent_sequence = self.agent_sequence.wrapping_add(1);

        // RC-301: when the session's authoritative committer is configured,
        // every event flows through the one commit boundary — the sequencer
        // assigns the final sequence, durable events are persisted with it,
        // and the committed envelope is published only when persistence
        // succeeded (strict policy) or explicitly degraded (best-effort).
        if let Some(committer) = &self.committer {
            match committer.commit(envelope).await {
                Ok(Some(committed)) => {
                    // RC-302: a terminal run triggers an automatic checkpoint
                    // at the committed sequence.
                    if matches!(
                        &committed.envelope.event,
                        AgentEvent::Completed { .. } | AgentEvent::Failed { .. }
                    ) {
                        if let Some(sequence) = committed.envelope.session_sequence {
                            committer.checkpoint_for_terminal_run(sequence);
                        }
                    }
                    let _ = self.task.events.send(committed.envelope.clone());
                    self.event_sink.send(committed.envelope);
                }
                Ok(None) => {
                    // Strict policy: persistence failed, so the event is
                    // never published. The session state has already
                    // advanced; the runtime must treat this as a
                    // session-level failure.
                    tracing::error!(
                        session_id = %self.agent.session_id,
                        agent_id = %self.agent.id,
                        "commit boundary rejected an event; it was not published"
                    );
                    self.fail_fatally(
                        "PERSISTENCE_COMMIT_REJECTED",
                        "strict event commit rejected publication".into(),
                    );
                    return false;
                }
                Err(error) => {
                    tracing::error!(
                        session_id = %self.agent.session_id,
                        agent_id = %self.agent.id,
                        %error,
                        "commit boundary failed; event was not published"
                    );
                    self.fail_fatally(
                        "PERSISTENCE_COMMIT_FAILED",
                        format!("strict event commit failed: {error}"),
                    );
                    return false;
                }
            }
            return true;
        }

        // Legacy path (no store configured): persist durable events directly
        // and let the session bus assign sequences.
        if harness_session_store::is_durable(&envelope.event) {
            self.persist(SessionMutation::AppendEvent(envelope.clone()))
                .await;
        }

        let _ = self.task.events.send(envelope.clone());
        self.event_sink.send(envelope);
        true
    }

    fn fail_fatally(&mut self, code: &str, message: String) {
        self.fatal_error = true;
        for (_, token) in self.backend_tokens.drain() {
            token.cancel();
        }
        for (_, token) in self.tool_tokens.drain() {
            token.cancel();
        }
        let error = AgentError {
            message,
            code: code.into(),
            details: None,
        };
        self.agent.state.status = AgentStatus::Failed;
        self.agent.state.active_run = None;
        self.agent.state.pending_tools.clear();
        self.agent.state.pending_permissions.clear();
        self.agent.state.last_error = Some(error.clone());
        self.final_result = Some(Err(error));
        self.publish_status();
    }

    /// Persists a session mutation through the injected session store.
    async fn persist(&mut self, mutation: SessionMutation) {
        let Some(store) = &self.session_store else {
            tracing::warn!(?mutation, "Persist requested without a session store");
            return;
        };

        if let Err(error) = validate_transcript(&self.agent.state.messages) {
            tracing::error!(?error, "refusing to persist mutation for invalid transcript");
            return;
        }

        match mutation {
            SessionMutation::AppendEvent(envelope) => {
                if let Err(error) = store.append(envelope.into()).await {
                    tracing::error!(?error, "failed to append durable session event");
                }
            }
            SessionMutation::SaveSnapshot(payload) => {
                match serde_json::from_value::<harness_session_store::DurableSessionSnapshot>(
                    payload,
                ) {
                    Ok(snapshot) => {
                        if let Err(error) = store.save_snapshot(snapshot).await {
                            tracing::error!(?error, "failed to save session snapshot");
                        }
                    }
                    Err(error) => {
                        tracing::error!(?error, "dropping invalid session snapshot payload");
                    }
                }
            }
        }
    }

    /// Streams a backend request into the agent's mailbox.
    ///
    /// The transcript is validated first; an invalid transcript is rejected
    /// with a normal backend error. Otherwise a forwarding task relays
    /// streamed events to the mailbox while a driver task holds the scheduler
    /// permits and runs the request. The driver waits for the forwarding task
    /// to drain before synthesizing a terminal event, so streamed events are
    /// never overtaken.
    async fn execute_backend(&mut self, request: ExecutionRequest) {
        if let Err(error) = validate_transcript(&self.agent.state.messages) {
            tracing::error!(?error, "refusing to dispatch backend request with invalid transcript");
            let event = ExecutionEvent::Error {
                request_id: request.request_id,
                error: ExecutionError::InvalidRequest {
                    message: format!("invalid transcript: {error}"),
                },
            };
            let _ = self
                .task
                .commands_tx
                .send(AgentCommand::BackendEvent {
                    run_id: request.run_id,
                    event,
                })
                .await;
            return;
        }

        let backend_id = self.backend.descriptor().id;
        let run_id = request.run_id;
        let request_id = request.request_id;
        let (event_tx, mut event_rx) = broadcast::channel(256);
        let token = self.cancel.child_token();
        self.backend_tokens.insert(run_id, token.clone());

        let commands = self.task.commands_tx.clone();
        let forward_cancel = self.cancel.clone();
        let forward_handle = tokio::spawn(async move {
            let mut terminal_forwarded = false;
            loop {
                tokio::select! {
                    event = event_rx.recv() => match event {
                        Ok(event) => {
                            terminal_forwarded |= matches!(
                                &event,
                                ExecutionEvent::Completed { .. } | ExecutionEvent::Error { .. }
                            );
                            if commands.send(AgentCommand::BackendEvent { run_id, event }).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(run_id = ?run_id, skipped = n, "backend event receiver lagged");
                        }
                    },
                    _ = forward_cancel.cancelled() => break,
                }
            }
            terminal_forwarded
        });

        let backend = self.backend.clone();
        let scheduler = self.scheduler.clone();
        let result_commands = self.task.commands_tx.clone();
        let result_token = token.clone();
        tokio::spawn(async move {
            // Capacity waits must never block the agent mailbox. Both waits
            // are also run-cancellable, so a queued request cannot survive a
            // cancellation and execute later.
            let _global_permit = tokio::select! {
                permit = scheduler.acquire_backend_permit() => permit,
                _ = result_token.cancelled() => return,
            };
            let _backend_guard = tokio::select! {
                guard = scheduler.acquire_backend_specific_permit(backend_id) => guard,
                _ = result_token.cancelled() => return,
            };
            let outcome = backend.execute(request, event_tx, result_token).await;
            let terminal_forwarded = forward_handle.await.unwrap_or(false);
            if !terminal_forwarded {
                let event = match outcome {
                    Ok(result) => ExecutionEvent::Completed { request_id, result },
                    Err(error) => ExecutionEvent::Error { request_id, error },
                };
                let _ = result_commands
                    .send(AgentCommand::BackendEvent { run_id, event })
                    .await;
            }
        });
    }

    /// Executes a tool call and reports the result to the agent's mailbox.
    async fn execute_tool(&mut self, request: ToolRequest) {
        let call_id = request.call.id;
        let name = request.call.name.clone();
        let arguments = request.call.arguments.clone();

        // M5: `agent.spawn` is not a registered `ToolExecutor` — its
        // implementation needs `&mut self` (to call `spawn_agent`, which
        // applies `ChildSpawned`/`ChildFailed` against the deterministic
        // core) that the generic path below cannot give it, since normal
        // tool execution runs in a detached `tokio::spawn`'d task with no
        // access back into the runner. Intercepted here, before any
        // registry lookup, and always routed through the exact same
        // `spawn_agent` the plain `AgentEffect::SpawnAgent` path already
        // uses — a model can no more bypass parent budgets, tool
        // delegation rules, or depth limits through this tool than existing
        // Rust-orchestrated spawning could.
        if name == crate::spawn_tool::AGENT_SPAWN_TOOL_NAME {
            self.execute_spawn_tool(call_id, &arguments).await;
            return;
        }

        let token = self.cancel.child_token();
        self.tool_tokens.insert(call_id, token.clone());
        let commands = self.task.commands_tx.clone();
        let executor = self.tool_registry.get_executor(&name);
        let scheduler = self.scheduler.clone();

        // M3: `shell.exec` (and any future tool with the same shape) forks a
        // real OS process, which is a heavier resource than "a tool call" in
        // general — bound it separately via `max_concurrent_processes` in
        // addition to the generic tool-execution permit below, so a burst of
        // shell calls can't spawn unboundedly many processes even while
        // comfortably inside `max_concurrent_tool_executions`.
        let spawns_process = name == "shell.exec";
        let tool_name_for_metrics = name.clone();

        tokio::spawn(async move {
            // Like provider capacity, tool capacity is a cancellable queue
            // wait and must not stall the runner's command mailbox.
            let _permit = tokio::select! {
                permit = scheduler.acquire_tool_permit() => permit,
                _ = token.cancelled() => return,
            };
            let _process_permit = if spawns_process {
                match scheduler.acquire_process_permit_cancellable(&token).await {
                    Some(permit) => Some(permit),
                    None => return,
                }
            } else {
                None
            };
            let Some(executor) = executor else {
                let _ = commands
                    .send(AgentCommand::ToolFailed {
                        call_id,
                        error: harness_protocol::tools::ToolError::ExecutionFailed,
                    })
                    .await;
                return;
            };

            let input = harness_tools::ToolInput { arguments };
            let tool_call_start = std::time::Instant::now();
            let outcome = executor.execute(input, token).await;
            metrics::histogram!("harness_tool_call_duration_seconds", "tool" => tool_name_for_metrics.clone())
                .record(tool_call_start.elapsed().as_secs_f64());
            metrics::counter!(
                "harness_tool_calls_total",
                "tool" => tool_name_for_metrics,
                "outcome" => if outcome.is_ok() { "success" } else { "error" }
            )
            .increment(1);
            match outcome {
                Ok(tool_result) => {
                    let result = harness_protocol::tools::ToolResult {
                        call_id,
                        output: tool_result.output,
                        is_error: tool_result.is_error,
                    };
                    let _ = commands
                        .send(AgentCommand::ToolCompleted { call_id, result })
                        .await;
                }
                Err(tool_error) => {
                    let error = to_protocol_tool_error(tool_error);
                    let _ = commands
                        .send(AgentCommand::ToolFailed { call_id, error })
                        .await;
                }
            }
        });
    }

    /// M5: `agent.spawn`'s implementation. Always resolves via
    /// `AgentCommand::ToolCompleted` — never `ToolFailed` — for any spawn
    /// rejection, whether that's bad arguments, a tool it can't be granted,
    /// or the supervisor rejecting a budget/depth/capability request
    /// (`SupervisorError`, e.g. `SpawnNotAllowed`, budget escalation, depth
    /// exceeded). All of these are meaningful, actionable information for
    /// the calling model (e.g. "your budget request escalates past your
    /// own ceiling" is something a model should see and correct), unlike
    /// `ToolError`'s coarse fixed enum, which has no room for a real
    /// message and exists for infrastructure failures the model can't
    /// reason about — mirroring how every other tool in this workspace
    /// reports its own domain errors as `ToolResult { is_error: true,
    /// output: <message> }` rather than `ToolFailed`.
    async fn execute_spawn_tool(&mut self, call_id: ToolCallId, arguments: &serde_json::Value) {
        let (spec, warnings) = match crate::spawn_tool::build_spawn_spec(arguments, &self.agent) {
            Ok(parsed) => parsed,
            Err(message) => {
                let effects = self.apply_and_publish(AgentCommand::ToolCompleted {
                    call_id,
                    result: harness_protocol::tools::ToolResult {
                        call_id,
                        output: serde_json::json!({ "error": message }),
                        is_error: true,
                    },
                });
                Box::pin(self.dispatch_effects(effects)).await;
                return;
            }
        };

        match self.spawn_agent(spec).await {
            Ok(outcome) => {
                let (is_error, mut output) = match &outcome.awaited {
                    None => (
                        false,
                        serde_json::json!({
                            "child_agent_id": outcome.child_id.to_string(),
                            "status": "spawned",
                            "note": "running concurrently; its own completion will appear as ChildAgentCompleted",
                        }),
                    ),
                    Some(Ok(result)) => (
                        false,
                        serde_json::json!({
                            "child_agent_id": outcome.child_id.to_string(),
                            "status": "completed",
                            "summary": result.summary,
                        }),
                    ),
                    Some(Err(message)) => (
                        true,
                        serde_json::json!({
                            "child_agent_id": outcome.child_id.to_string(),
                            "status": "failed",
                            "error": message,
                        }),
                    ),
                };
                if !warnings.is_empty() {
                    output["warnings"] = serde_json::json!(warnings);
                }
                let effects = self.apply_and_publish(AgentCommand::ToolCompleted {
                    call_id,
                    result: harness_protocol::tools::ToolResult { call_id, output, is_error },
                });
                Box::pin(self.dispatch_effects(effects)).await;
            }
            Err(message) => {
                tracing::warn!(%message, "agent.spawn tool call could not spawn a child");
                let effects = self.apply_and_publish(AgentCommand::ToolCompleted {
                    call_id,
                    result: harness_protocol::tools::ToolResult {
                        call_id,
                        output: serde_json::json!({ "error": message }),
                        is_error: true,
                    },
                });
                Box::pin(self.dispatch_effects(effects)).await;
            }
        }
    }

    fn cancel_backend(&mut self, run_id: RunId) {
        if let Some(token) = self.backend_tokens.remove(&run_id) {
            token.cancel();
        }
    }

    fn cancel_tool(&mut self, call_id: ToolCallId) {
        if let Some(token) = self.tool_tokens.remove(&call_id) {
            token.cancel();
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.agent.state.status,
            AgentStatus::Cancelled | AgentStatus::Completed | AgentStatus::Failed
        )
    }
}

fn to_protocol_tool_error(
    error: harness_tools::ToolError,
) -> harness_protocol::tools::ToolError {
    match error {
        harness_tools::ToolError::ExecutionFailed => {
            harness_protocol::tools::ToolError::ExecutionFailed
        }
        harness_tools::ToolError::PermissionDenied => {
            harness_protocol::tools::ToolError::PermissionDenied
        }
        harness_tools::ToolError::Timeout => harness_protocol::tools::ToolError::Timeout,
        harness_tools::ToolError::Internal => harness_protocol::tools::ToolError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use harness_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
    use harness_protocol::backend::{
        BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
    };
    use harness_protocol::commands::UserInput;
    use harness_protocol::ids::{BackendId, ConfigurationId, IntegrationId, SessionId};
    use harness_protocol::tools::AgentToolset;
    use harness_protocol::usage::AgentBudget;

    use crate::scheduler::{Scheduler, SchedulerConfig};
    use crate::testing::{FakeBackend, FakeToolExecutor, FakeToolRegistry};
    use crate::workspace::FakeWorkspace;

    use super::*;

    #[tokio::test]
    async fn send_command_through_mailbox() {
        let (mut task, sender) = AgentTask::new(AgentId::new());
        sender
            .send(AgentCommand::Cancel)
            .await
            .expect("send command");
        assert!(matches!(
            task.commands.recv().await,
            Some(AgentCommand::Cancel)
        ));
    }

    struct NoopSink;
    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    fn test_agent(agent_id: AgentId, session_id: SessionId) -> Agent {
        Agent::new(
            agent_id,
            session_id,
            None,
            0,
            String::new(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset {
                    tools: StdHashMap::new(),
                },
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: WorkspaceCapabilities {
                    can_read: false,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        )
    }

    /// Builds a test agent with a single tool whose policy requires
    /// permission (`PermissionMode::Ask`), used to exercise the
    /// `WaitingForPermission` state.
    fn test_agent_with_ask_permission_tool(
        agent_id: AgentId,
        session_id: SessionId,
        tool_name: &str,
    ) -> Agent {
        let tool_id = harness_protocol::ids::ToolId::new();
        let mut tools = StdHashMap::new();
        tools.insert(
            tool_id,
            harness_protocol::tools::ToolCapability {
                descriptor: harness_protocol::tools::ToolDescriptor {
                    id: tool_id,
                    name: tool_name.into(),
                    description: "ask-permission test tool".into(),
                    input_schema: serde_json::json!({}),
                },
                policy: harness_protocol::tools::ToolPolicy {
                    permission: harness_protocol::tools::PermissionMode::Ask,
                    enabled: true,
                },
                delegatable: false,
            },
        );
        Agent::new(
            agent_id,
            session_id,
            None,
            0,
            String::new(),
            BackendBinding {
                reference: BackendReference {
                    integration: IntegrationId::new(),
                    configuration: ConfigurationId::new(),
                    model: None,
                },
                descriptor: BackendDescriptor {
                    id: BackendId::new(),
                    name: "fake".into(),
                    description: "fake".into(),
                    capabilities: BackendCapabilities::default(),
                },
            },
            AgentCapabilities {
                tools: AgentToolset { tools },
                can_spawn_agents: false,
                max_child_depth: None,
                workspace: WorkspaceCapabilities {
                    can_read: false,
                    can_write: false,
                    can_search: false,
                },
                backend: BackendCapabilities::default(),
            },
            AgentBudget::default(),
        )
    }

    /// Task 2.3 acceptance criterion: start a `FakeBackend` call that blocks
    /// until cancelled, cancel the token mid-flight, and assert the runner
    /// transitions the agent to `Cancelled` and stops emitting further
    /// backend events.
    #[tokio::test]
    async fn cancel_mid_flight_backend_call_transitions_to_cancelled() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);

        let (task, sender) = AgentTask::new(agent_id);
        let backend = Arc::new(FakeBackend::new().blocking_until_cancelled());
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        );

        let mut events_rx = runner.task.events.subscribe();

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "hi".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send StartRun");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        // Give the runner time to process StartRun and spawn the (blocking)
        // backend call before we cancel mid-flight.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Mid-flight cancellation.
        cancel.cancel();

        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should stop promptly after cancellation")
            .expect("runner task should not panic");

        assert_eq!(
            runner.agent.state.status,
            AgentStatus::Cancelled,
            "agent should transition to Cancelled after mid-flight cancellation"
        );

        // The live-state table should also reflect Cancelled.
        let live = live_state
            .lock()
            .expect("live_state mutex poisoned")
            .get(&agent_id)
            .cloned()
            .expect("live state entry exists");
        assert_eq!(live.status, AgentStatus::Cancelled);

        while events_rx.try_recv().is_ok() {}
        assert!(matches!(
            events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    /// RST-002: a runner opted into `.long_lived(true)` must keep processing
    /// its mailbox after a run finishes, so the same agent/session can
    /// accept a second `StartRun` without being recreated.
    #[tokio::test]
    async fn long_lived_runner_accepts_a_second_start_run_after_completion() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);

        let (task, sender) = AgentTask::new(agent_id);
        let request_id = harness_protocol::ids::RequestId::new();
        let backend = Arc::new(FakeBackend::new().with_result(
            harness_protocol::backend::ExecutionResult {
                request_id,
                usage: harness_protocol::usage::ModelUsage::default(),
                cost: harness_protocol::usage::Cost::default(),
                finish_reason: "end_turn".into(),
            },
        ));
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        )
        .long_lived(true);

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "first".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send first StartRun");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        // Wait for the first run to complete (status returns to Idle with a
        // recorded Success outcome) while the mailbox loop is still alive.
        let mut first_completed = false;
        for _ in 0..100 {
            let live = live_state.lock().expect("live_state mutex poisoned").get(&agent_id).cloned();
            if let Some(live) = live {
                if live.status == AgentStatus::Idle
                    && matches!(live.last_outcome, Some(AgentOutcome::Success))
                {
                    first_completed = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(first_completed, "first run should complete");

        // A second StartRun on the same mailbox must still be accepted and
        // processed — proving the runner task did not exit after the first
        // run's FinishRun effect.
        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "second".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send second StartRun on the still-alive mailbox");

        let mut second_completed = false;
        for _ in 0..100 {
            let live = live_state.lock().expect("live_state mutex poisoned").get(&agent_id).cloned();
            if let Some(live) = live {
                if live.status == AgentStatus::Idle && live.total_requests >= 2 {
                    second_completed = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            second_completed,
            "a long-lived runner must process a second StartRun on the same mailbox"
        );

        drop(sender);
        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should exit once the mailbox is closed")
            .expect("runner task should not panic");
        assert_eq!(runner.agent.state.status, AgentStatus::Idle);
    }

    #[tokio::test]
    async fn cancel_is_processed_while_backend_waits_for_scheduler_capacity() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, sender) = AgentTask::new(agent_id);
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
            max_concurrent_backend_requests: 0,
            ..SchedulerConfig::default()
        }));
        let mut runner = AgentRunner::new(
            agent,
            task,
            Arc::new(FakeBackend::new().blocking_until_cancelled()),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel,
            live_state.clone(),
            scheduler,
        );

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "wait for capacity".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("start run");
        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if live_state
                    .lock()
                    .expect("live_state mutex poisoned")
                    .get(&agent_id)
                    .is_some_and(|state| state.status == AgentStatus::PreparingContext)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should reach its scheduler-capacity wait");
        sender.send(AgentCommand::Cancel).await.expect("cancel run");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if live_state
                    .lock()
                    .expect("live_state mutex poisoned")
                    .get(&agent_id)
                    .is_some_and(|state| state.status == AgentStatus::Cancelled)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capacity wait must not block cancellation");

        drop(sender);
        let runner = tokio::time::timeout(Duration::from_secs(1), run_handle)
            .await
            .expect("cancelled capacity wait should release mailbox senders")
            .expect("runner task should not panic");
        assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
    }

    #[tokio::test]
    async fn strict_commit_failure_terminates_with_truthful_failed_state() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, sender) = AgentTask::new(agent_id);
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let store = Arc::new(harness_session_store::testing::MemoryStore::new());
        store.set_fail_appends(true);
        let committer = Arc::new(SessionCommitter::new(store, session_id));
        let mut runner = AgentRunner::new(
            agent,
            task,
            Arc::new(FakeBackend::new().blocking_until_cancelled()),
            Arc::new(FakeToolRegistry::new()),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            CancellationToken::new(),
            live_state.clone(),
            Arc::new(Scheduler::new(SchedulerConfig::default())),
        )
        .with_committer(committer)
        .long_lived(true);

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "must be durable".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("start run");
        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        let mut runner = tokio::time::timeout(Duration::from_secs(1), run_handle)
            .await
            .expect("strict persistence failure must terminate the runner")
            .expect("runner task should not panic");
        assert_eq!(runner.agent.state.status, AgentStatus::Failed);
        assert!(runner.agent.state.active_run.is_none());
        let error = runner
            .take_final_result()
            .expect("fatal result")
            .expect_err("strict persistence failure must be an error");
        assert_eq!(error.code, "PERSISTENCE_COMMIT_FAILED");
        assert_eq!(
            live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .expect("live state")
                .status,
            AgentStatus::Failed
        );
    }

    /// M2: a `FollowUp` sent while a run is active must queue FIFO
    /// (`Agent::queued_inputs`) rather than starting immediately, and the
    /// long-lived runner's automatic `StartNextQueuedRun` drain must start
    /// it only after the first run genuinely finishes.
    #[tokio::test]
    async fn follow_up_queued_while_run_active_starts_after_first_completes() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, sender) = AgentTask::new(agent_id);
        let request_id = harness_protocol::ids::RequestId::new();
        let backend = Arc::new(
            FakeBackend::new()
                .with_events(vec![ExecutionEvent::TextDelta {
                    request_id,
                    delta: "partial".into(),
                }])
                .with_result(harness_protocol::backend::ExecutionResult {
                    request_id,
                    usage: harness_protocol::usage::ModelUsage::default(),
                    cost: harness_protocol::usage::Cost::default(),
                    finish_reason: "end_turn".into(),
                }),
        );
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        )
        .long_lived(true);

        let mut events_rx = runner.task.events.subscribe();

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "first".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send first StartRun");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        // Wait until the first run is genuinely in flight (Streaming) before
        // sending the follow-up, so it is guaranteed to be queued rather
        // than started immediately.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if live_state
                    .lock()
                    .expect("live_state mutex poisoned")
                    .get(&agent_id)
                    .is_some_and(|state| state.status == AgentStatus::Streaming)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first run should reach Streaming");

        sender
            .send(AgentCommand::FollowUp {
                input: UserInput {
                    text: "second".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send follow-up while first run is active");

        // Collect events in order until we observe two RunStarted events,
        // proving the follow-up was queued (not started immediately) and
        // only began after the first run's Completed/FinishRun.
        let mut run_started_count = 0;
        let mut saw_first_completed_before_second_start = false;
        let mut first_completed_seen = false;
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Ok(envelope) = events_rx.recv().await {
                match envelope.event {
                    AgentEvent::Completed { .. } => {
                        first_completed_seen = true;
                    }
                    AgentEvent::RunStarted { .. } => {
                        run_started_count += 1;
                        if run_started_count == 2 {
                            saw_first_completed_before_second_start = first_completed_seen;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("should observe a second RunStarted for the queued follow-up");

        assert_eq!(run_started_count, 2, "the follow-up must eventually start its own run");
        assert!(
            saw_first_completed_before_second_start,
            "the queued follow-up must not start until the first run completed"
        );

        drop(sender);
        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should exit once the mailbox is closed")
            .expect("runner task should not panic");
        assert!(
            runner.agent.state.queued_inputs.is_empty(),
            "the queue must be drained once the follow-up run has started"
        );
    }

    /// M2: the runner's long-lived loop drains the next queued input
    /// (`StartNextQueuedRun`) after *any* `FinishRun` effect, not just a
    /// successful completion — `fail()` emits `FinishRun` exactly like a
    /// success or a cancellation does. This locks that behavior down for the
    /// failure case specifically: a follow-up queued while a run is active
    /// must still start its own run after the first run fails, not be
    /// stranded in `queued_inputs` forever.
    #[tokio::test]
    async fn follow_up_queued_while_run_active_starts_after_first_run_fails() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, sender) = AgentTask::new(agent_id);
        let backend = Arc::new(FakeBackend::new().with_error(
            harness_protocol::backend::ExecutionError::BackendError {
                message: "simulated transient failure".into(),
                code: "TEMPORARY".into(),
            },
        ));
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        )
        .long_lived(true);

        let mut events_rx = runner.task.events.subscribe();

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "first".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send first StartRun");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if live_state
                    .lock()
                    .expect("live_state mutex poisoned")
                    .get(&agent_id)
                    .is_some_and(|state| state.status == AgentStatus::PreparingContext)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first run should reach PreparingContext");

        sender
            .send(AgentCommand::FollowUp {
                input: UserInput {
                    text: "second".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("queue a follow-up while the first run is active");

        let mut run_started_count = 0;
        let mut saw_first_failed_before_second_start = false;
        let mut first_failed_seen = false;
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Ok(envelope) = events_rx.recv().await {
                match envelope.event {
                    AgentEvent::Failed { .. } => {
                        first_failed_seen = true;
                    }
                    AgentEvent::RunStarted { .. } => {
                        run_started_count += 1;
                        if run_started_count == 2 {
                            saw_first_failed_before_second_start = first_failed_seen;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("should observe a second RunStarted for the queued follow-up after the failure");

        assert_eq!(
            run_started_count, 2,
            "the follow-up must eventually start its own run after the first run fails"
        );
        assert!(
            saw_first_failed_before_second_start,
            "the queued follow-up must not start until the first run has failed"
        );

        drop(sender);
        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should exit once the mailbox is closed")
            .expect("runner task should not panic");
        assert!(
            runner.agent.state.queued_inputs.is_empty(),
            "the queue must be drained once the follow-up run has started"
        );
    }

    /// M3: `shell.exec` calls must be bounded separately from generic tool
    /// concurrency via `SchedulerConfig::max_concurrent_processes` — a real
    /// OS process is heavier than "a tool call" in general. This proves the
    /// wiring in `execute_tool` specifically, not just that the underlying
    /// semaphore serializes (that's a given): with tool-execution
    /// concurrency generous (10) but process concurrency exhausted (1, held
    /// by an in-flight `shell.exec` call), a second `shell.exec` call must
    /// block *before* ever reaching the tool executor. We prove that by
    /// cancelling the second call's own token while it's still blocked: if
    /// it were gated correctly, cancellation resolves the scheduler wait
    /// with `None` and the call returns without ever invoking the executor
    /// — so no `ToolCompleted`/`ToolFailed` command is ever sent for it. If
    /// it were *not* gated (the bug this test guards against), the second
    /// call would have already reached the executor and blocked on
    /// `cancel.cancelled()` *inside* `FakeToolExecutor`, which resolves
    /// cancellation by sending `ToolFailed` — a directly observable
    /// difference.
    #[tokio::test]
    async fn shell_exec_calls_are_bounded_by_max_concurrent_processes() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, _sender) = AgentTask::new(agent_id);

        let mut registry = FakeToolRegistry::new();
        registry.add_executor(Arc::new(
            FakeToolExecutor::new(harness_tools::ToolDescriptor {
                id: harness_tools::ToolId::new("shell.exec"),
                name: "shell.exec".into(),
                description: "fake shell.exec".into(),
                input_schema: serde_json::json!({}),
            })
            .blocking_until_cancelled(),
        ));

        let scheduler = Arc::new(Scheduler::new(SchedulerConfig {
            max_concurrent_tool_executions: 10,
            max_concurrent_processes: 1,
            ..SchedulerConfig::default()
        }));
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            Arc::new(FakeBackend::new()),
            Arc::new(registry),
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel,
            live_state,
            scheduler,
        );

        let first_call = harness_protocol::ids::ToolCallId::new();
        runner
            .execute_tool(ToolRequest {
                call: harness_protocol::tools::ToolCall {
                    id: first_call,
                    name: "shell.exec".into(),
                    arguments: serde_json::json!({}),
                },
                permission: harness_protocol::tools::PermissionMode::Allow,
            })
            .await;

        // Give the first call's spawned task time to actually acquire both
        // permits and enter the executor's blocking wait.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second_call = harness_protocol::ids::ToolCallId::new();
        runner
            .execute_tool(ToolRequest {
                call: harness_protocol::tools::ToolCall {
                    id: second_call,
                    name: "shell.exec".into(),
                    arguments: serde_json::json!({}),
                },
                permission: harness_protocol::tools::PermissionMode::Allow,
            })
            .await;

        // Give the second call's spawned task time to reach (and block on)
        // whatever it's actually going to block on.
        tokio::time::sleep(Duration::from_millis(50)).await;
        runner.cancel_tool(second_call);

        // Drain the mailbox briefly: if the second call had reached the
        // executor (not gated), cancelling it produces a ToolFailed for
        // `second_call` promptly.
        let saw_second_call_complete = tokio::time::timeout(
            Duration::from_millis(200),
            runner.task.commands.recv(),
        )
        .await;
        match saw_second_call_complete {
            Ok(Some(AgentCommand::ToolFailed { call_id, .. }))
            | Ok(Some(AgentCommand::ToolCompleted { call_id, .. }))
                if call_id == second_call =>
            {
                panic!(
                    "the second shell.exec call reached the executor instead of blocking \
                     on the exhausted process permit — max_concurrent_processes is not enforced"
                );
            }
            _ => {}
        }
    }

    /// M2: `AgentCommand::Cancel` is documented as cancelling only the
    /// *current run* (see `AgentCommand::Cancel`'s doc comment and RC-203's
    /// `multiple_follow_ups_are_fifo_and_survive_cancellation`). A follow-up
    /// queued while a run was active is already-committed user intent and
    /// must survive session/runner-level cancellation too, not just an
    /// explicit `AgentCommand::Cancel` applied directly to the core state
    /// machine — otherwise a queued follow-up typed just before a crash or a
    /// host-initiated teardown would be silently lost, which the `Agent`
    /// value itself must not misrepresent to a caller or restore path that
    /// inspects it afterward.
    #[tokio::test]
    async fn queued_follow_up_survives_runner_cancellation() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let agent = test_agent(agent_id, session_id);
        let (task, sender) = AgentTask::new(agent_id);
        let backend = Arc::new(FakeBackend::new().blocking_until_cancelled());
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        )
        .long_lived(true);

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "first".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("send first StartRun");
        sender
            .send(AgentCommand::FollowUp {
                input: UserInput {
                    text: "queued".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("queue a follow-up while the first run is active");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should stop promptly after cancellation")
            .expect("runner task should not panic");

        assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
        assert_eq!(
            runner.agent.state.queued_inputs.len(),
            1,
            "cancellation must not discard already-queued follow-up/steer input"
        );
        assert_eq!(
            runner.agent.state.queued_inputs[0].text, "queued",
            "the surviving queued input must be the one sent before cancellation"
        );
    }

    /// M2: cancellation must be processed while the agent is
    /// `WaitingForPermission`, and must not leave stale `pending_permissions`
    /// or `pending_tools` state behind.
    #[tokio::test]
    async fn cancel_while_waiting_for_permission_transitions_to_cancelled() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let tool_name = "ask.tool";
        let agent = test_agent_with_ask_permission_tool(agent_id, session_id, tool_name);
        let (task, sender) = AgentTask::new(agent_id);
        let call_id = harness_protocol::ids::ToolCallId::new();
        let request_id = harness_protocol::ids::RequestId::new();
        let backend = Arc::new(
            FakeBackend::new()
                .with_events(vec![ExecutionEvent::ToolCallRequested {
                    request_id,
                    call: harness_protocol::tools::ToolCall {
                        id: call_id,
                        name: tool_name.into(),
                        arguments: serde_json::json!({}),
                    },
                }])
                .blocking_until_cancelled(),
        );
        let tool_registry = Arc::new(FakeToolRegistry::new());
        let cancel = CancellationToken::new();
        let live_state: LiveStateTable = Arc::new(Mutex::new(StdHashMap::new()));
        let scheduler = Arc::new(Scheduler::new(SchedulerConfig::default()));

        let mut runner = AgentRunner::new(
            agent,
            task,
            backend,
            tool_registry,
            Arc::new(FakeWorkspace::new()),
            Arc::new(NoopSink),
            cancel.clone(),
            live_state.clone(),
            scheduler,
        );

        sender
            .send(AgentCommand::StartRun {
                input: UserInput {
                    text: "please".into(),
                    attachments: vec![],
                },
            })
            .await
            .expect("start run");

        let run_handle = tokio::spawn(async move {
            runner.run().await;
            runner
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if live_state
                    .lock()
                    .expect("live_state mutex poisoned")
                    .get(&agent_id)
                    .is_some_and(|state| state.status == AgentStatus::WaitingForPermission)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should reach WaitingForPermission");

        cancel.cancel();

        let runner = tokio::time::timeout(Duration::from_secs(2), run_handle)
            .await
            .expect("runner should stop promptly after cancellation while waiting for permission")
            .expect("runner task should not panic");

        assert_eq!(runner.agent.state.status, AgentStatus::Cancelled);
        assert!(runner.agent.state.pending_permissions.is_empty());
        assert!(runner.agent.state.pending_tools.is_empty());
        assert_eq!(
            live_state
                .lock()
                .expect("live_state mutex poisoned")
                .get(&agent_id)
                .expect("live state")
                .status,
            AgentStatus::Cancelled
        );
    }
}

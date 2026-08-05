//! Async runtime loop for a single deterministic agent.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::transcript::validate_transcript;
use harness_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::{AgentCommand, AgentError, AgentResult, AgentStatus};
use harness_protocol::effects::{AgentEffect, SessionMutation, SpawnAgentSpec, SpawnMode, ToolRequest};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp, ToolCallId};

use crate::agent_supervisor::{AgentSupervisor, SupervisorError};
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::session_runtime::LiveStateTable;
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
            backend_tokens: HashMap::new(),
            tool_tokens: HashMap::new(),
            final_result: None,
            supervision: None,
            session_store: None,
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
                }
            };

            if self.cancel.is_cancelled() {
                self.cancel_run().await;
                break;
            }

            let effects = self.apply_and_publish(command);
            let finished_run = self.dispatch_effects(effects).await;
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
        let child_usage = self
            .agent
            .usage
            .child_usage
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let usage = harness_core::usage::compute_agent_usage_summary(
            &self.agent.usage.records,
            &child_usage,
        );
        let total_cost_usd =
            self.agent
                .usage
                .records
                .iter()
                .fold(None::<Decimal>, |acc, record| {
                    match record.cost.amount_usd {
                        Some(amount) => Some(acc.unwrap_or(Decimal::ZERO) + amount),
                        None => acc,
                    }
                });

        let mut table = self.live_state.lock().expect("live_state mutex poisoned");
        let entry = table.entry(self.agent.id).or_default();
        entry.status = self.agent.state.status;
        entry.current_operation = self.agent.state.current_operation.clone();
        entry.last_error = self.agent.state.last_error.clone();
        entry.usage = usage;
        entry.total_requests = self.agent.usage.records.len() as u64;
        entry.total_cost_usd = total_cost_usd;
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
                AgentEffect::Emit { event } => self.emit(event).await,
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
                    self.spawn_agent(spec).await;
                }
                AgentEffect::RequestPermission { request } => {
                    tracing::warn!(?request, "RequestPermission is not wired in Phase 2");
                }
                AgentEffect::Persist { mutation } => {
                    self.persist(mutation).await;
                }
            }
        }
        finish
    }

    async fn spawn_agent(&mut self, spec: SpawnAgentSpec) {
        let Some(supervision) = self.supervision.clone() else {
            tracing::warn!(?spec, "SpawnAgent requested without a supervisor");
            return;
        };
        let mode = spec.mode;
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
                let awaiting = matches!(mode, SpawnMode::AwaitResult);
                let effects = self.apply_and_publish(AgentCommand::ChildSpawned {
                    agent_id: child_id,
                    awaiting,
                });
                Box::pin(self.dispatch_effects(effects)).await;
                if awaiting {
                    if let Err(error) = supervision.supervisor.await_child(child_id).await {
                        let effects = self.apply_and_publish(AgentCommand::ChildFailed {
                            agent_id: child_id,
                            error: AgentError {
                                message: error.to_string(),
                                code: "CHILD_AWAIT_FAILED".into(),
                                details: None,
                            },
                        });
                        Box::pin(self.dispatch_effects(effects)).await;
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "failed to spawn child agent"),
        }
    }

    async fn emit(&mut self, event: AgentEvent) {
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

        if harness_session_store::is_durable(&envelope.event) {
            self.persist(SessionMutation::AppendEvent(envelope.clone()))
                .await;
        }

        let _ = self.task.events.send(envelope.clone());
        self.event_sink.send(envelope);
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

        let global_permit = self.scheduler.acquire_backend_permit().await;
        let backend_id = self.backend.descriptor().id;
        let backend_guard = self
            .scheduler
            .acquire_backend_specific_permit(backend_id)
            .await;

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
        let result_commands = self.task.commands_tx.clone();
        let result_token = token.clone();
        tokio::spawn(async move {
            let _global_permit = global_permit;
            let _backend_guard = backend_guard;
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
        let permit = self.scheduler.acquire_tool_permit().await;

        let call_id = request.call.id;
        let name = request.call.name.clone();
        let arguments = request.call.arguments.clone();
        let token = self.cancel.child_token();
        self.tool_tokens.insert(call_id, token.clone());
        let commands = self.task.commands_tx.clone();
        let executor = self.tool_registry.get_executor(&name);

        tokio::spawn(async move {
            let _permit = permit;
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
            match executor.execute(input, token).await {
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
    use crate::testing::{FakeBackend, FakeToolRegistry};
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
}

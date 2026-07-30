//! Async runtime loop for a single deterministic agent.

use std::collections::HashMap;
use std::sync::Arc;

use rust_decimal::Decimal;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_protocol::backend::{ExecutionEvent, ExecutionRequest};
use harness_protocol::commands::{AgentCommand, AgentStatus};
use harness_protocol::effects::{AgentEffect, ToolRequest};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp, ToolCallId};

use crate::scheduler::Scheduler;
use crate::session_runtime::LiveStateTable;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry};

/// Mailbox and control handles for one agent.
pub struct AgentTask {
    pub id: AgentId,
    pub commands: mpsc::Receiver<AgentCommand>,
    /// Retained by the runner so asynchronous effect tasks can report results.
    pub commands_tx: mpsc::Sender<AgentCommand>,
    pub events: broadcast::Sender<AgentEventEnvelope>,
    pub cancel: CancellationToken,
}

impl AgentTask {
    /// Creates a mailbox and returns its external command sender.
    pub fn new(id: AgentId) -> (Self, mpsc::Sender<AgentCommand>) {
        let (commands_tx, commands) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let task = Self {
            id,
            commands,
            commands_tx: commands_tx.clone(),
            events,
            cancel: CancellationToken::new(),
        };
        (task, commands_tx)
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

/// Drives an agent state machine and dispatches its effects asynchronously.
pub struct AgentRunner {
    pub agent: Agent,
    pub task: AgentTask,
    pub backend: Arc<dyn ExecutionBackend>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub event_sink: Arc<dyn EventSink>,
    pub agent_sequence: u64,
    pub cancel: CancellationToken,
    /// Concurrency throttle for backend requests, tool executions, etc.
    pub scheduler: Arc<Scheduler>,
    /// Shared table that this runner publishes its live status/usage into
    /// after every transition, so external readers (e.g.
    /// `SessionRuntime::agent_live_state`) always see a pure, up-to-date
    /// projection rather than a stale copy.
    live_state: LiveStateTable,
    backend_tokens: HashMap<RunId, CancellationToken>,
    tool_tokens: HashMap<ToolCallId, CancellationToken>,
}

impl AgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: Agent,
        mut task: AgentTask,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
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
            event_sink,
            agent_sequence: 0,
            cancel,
            scheduler,
            live_state,
            backend_tokens: HashMap::new(),
            tool_tokens: HashMap::new(),
        }
    }

    /// Processes commands until completion, cancellation, or mailbox closure.
    pub async fn run(&mut self) {
        self.publish_status();
        loop {
            if self.cancel.is_cancelled() {
                if !self.is_terminal() {
                    let effects = self.apply_and_publish(AgentCommand::Cancel);
                    self.dispatch_effects(effects).await;
                }
                break;
            }

            let command = tokio::select! {
                command = self.task.commands.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
                _ = self.cancel.cancelled() => {
                    if !self.is_terminal() {
                        let effects = self.apply_and_publish(AgentCommand::Cancel);
                        self.dispatch_effects(effects).await;
                    }
                    break;
                }
            };

            if self.cancel.is_cancelled() {
                if !self.is_terminal() {
                    let effects = self.apply_and_publish(AgentCommand::Cancel);
                    self.dispatch_effects(effects).await;
                }
                break;
            }

            let effects = self.apply_and_publish(command);
            if self.dispatch_effects(effects).await {
                break;
            }
        }
    }

    /// Applies `command` to the agent and immediately publishes the
    /// resulting status/operation/usage to the shared live-state table.
    fn apply_and_publish(&mut self, command: AgentCommand) -> Vec<AgentEffect> {
        let effects = self.agent.apply(command);
        self.publish_status();
        effects
    }

    /// Writes the agent's current status, operation, error, and usage into
    /// the shared live-state table.
    fn publish_status(&self) {
        let usage = harness_core::usage::compute_agent_usage_summary(&self.agent.usage.records, &[]);
        let total_cost_usd = self.agent.usage.records.iter().fold(None::<Decimal>, |acc, record| {
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

    /// Records the outcome of a finished run (Success/Cancelled/Failed) in
    /// the shared live-state table.
    ///
    /// `AgentStatus` returns to `Idle` after a successful run, so this is
    /// the only durable record that a run finished versus never having run.
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
                AgentEffect::Emit { event } => self.emit(event),
                AgentEffect::FinishRun { .. } => finish = true,
                AgentEffect::CancelBackend { run_id } => self.cancel_backend(run_id),
                AgentEffect::CancelTool { call_id } => self.cancel_tool(call_id),
                AgentEffect::CancelChild { agent_id } => {
                    tracing::warn!(?agent_id, "CancelChild is not wired in Phase 2");
                }
                AgentEffect::SpawnAgent { spec } => {
                    tracing::warn!(?spec, "SpawnAgent is not wired in Phase 2");
                }
                AgentEffect::RequestPermission { request } => {
                    tracing::warn!(?request, "RequestPermission is not wired in Phase 2");
                }
                AgentEffect::Persist { mutation } => {
                    tracing::warn!(?mutation, "Persist is not wired in Phase 2");
                }
            }
        }
        finish
    }

    fn emit(&mut self, event: AgentEvent) {
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
        // Send to the agent's own event broadcast channel (read by SessionEventBus).
        let _ = self.task.events.send(envelope.clone());
        // Forward to the external event sink (persistence, logging, etc.).
        self.event_sink.send(envelope);
    }

    /// Dispatches a backend execution effect.
    ///
    /// Acquires **both** a global backend permit from the [`Scheduler`] and a
    /// backend-specific permit (concurrency + rate-limit guard) before
    /// spawning the forwarding and driver tasks.  The permits are moved into
    /// the driver task and held for the entire duration of the backend
    /// request, throttling concurrent LLM API calls across all agents and
    /// enforcing per-backend rate limits.
    ///
    /// Two tasks cooperate here:
    ///
    /// 1. A **forwarding** task that drains `event_rx` and forwards each
    ///    streamed `ExecutionEvent` into the agent's own mailbox as it
    ///    arrives, so intermediate deltas are observed as they occur.
    /// 2. A **driver** task that awaits `backend.execute(..)` to
    ///    completion, then — crucially — awaits the forwarding task's
    ///    `JoinHandle` before injecting the final `Result` only when the
    ///    backend did not already stream a terminal event.
    ///
    /// The await on the forwarding task's handle is what guarantees
    /// ordering: `backend.execute` drops its `sink` sender when it
    /// returns, which closes the broadcast channel, which is exactly the
    /// signal the forwarding task uses to stop. Without waiting for it,
    /// a backend whose `execute` future never yields (e.g. `FakeBackend`
    /// or a purely synchronous scripted backend) can have its driver task
    /// enqueue the synthesized terminal event in the mailbox *before* the
    /// forwarding task — spawned but not yet polled — has forwarded the
    /// already-buffered intermediate events, causing them to be silently
    /// dropped once the agent's `active_run` guard clears on the terminal
    /// event. Waiting for the forwarding task to finish first eliminates
    /// that race regardless of scheduler ordering.
    async fn execute_backend(&mut self, request: ExecutionRequest) {
        // Acquire the global backend semaphore permit.
        let global_permit = self.scheduler.acquire_backend_permit().await;

        // Acquire the backend-specific permit (concurrency slot + rate-limit
        // guard).  This is a no-op if no limits were configured for this
        // backend via Scheduler::configure_backend_limits.
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
            // Hold both permits for the complete backend request lifecycle.
            // The forwarding task must not lock these permits: it needs to
            // receive events while the driver is executing the backend.
            let _global_permit = global_permit;
            let _backend_guard = backend_guard;
            let outcome = backend.execute(request, event_tx, result_token).await;
            // Ensure every already-streamed event has been forwarded to the
            // mailbox before injecting the synthesized terminal event (see
            // the doc comment above for why this ordering matters).
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

    /// Dispatches a tool execution effect.
    ///
    /// Acquires a tool permit from the [`Scheduler`] before spawning the
    /// execution task. The permit is moved into the spawned task and held
    /// for the entire tool execution, throttling concurrent tool calls
    /// across all agents.
    async fn execute_tool(&mut self, request: ToolRequest) {
        // Throttle tool executions — block if at capacity.
        let permit = self.scheduler.acquire_tool_permit().await;

        let call_id = request.call.id;
        let name = request.call.name.clone();
        let arguments = request.call.arguments.clone();
        let token = self.cancel.child_token();
        self.tool_tokens.insert(call_id, token.clone());
        let commands = self.task.commands_tx.clone();

        match self.tool_registry.get_executor(&name) {
            Some(executor) => {
                tokio::spawn(async move {
                    // Hold the permit for the duration of tool execution.
                    let _permit = permit;
                    // Convert protocol ToolCall to harness-tools ToolInput
                    let input = harness_tools::ToolInput {
                        arguments,
                    };

                    match executor.execute(input, token).await {
                        Ok(tool_result) => {
                            // Convert harness-tools ToolResult to protocol ToolResult
                            let result = harness_protocol::tools::ToolResult {
                                call_id,
                                output: tool_result.output,
                                is_error: tool_result.is_error,
                            };
                            let _ = commands.send(AgentCommand::ToolCompleted { call_id, result }).await;
                        }
                        Err(tool_error) => {
                            // Convert harness-tools ToolError to protocol ToolError
                            let error = match tool_error {
                                harness_tools::ToolError::ExecutionFailed => {
                                    harness_protocol::tools::ToolError::ExecutionFailed
                                }
                                harness_tools::ToolError::PermissionDenied => {
                                    harness_protocol::tools::ToolError::PermissionDenied
                                }
                                harness_tools::ToolError::Timeout => {
                                    harness_protocol::tools::ToolError::Timeout
                                }
                                harness_tools::ToolError::Internal => {
                                    harness_protocol::tools::ToolError::Internal
                                }
                            };
                            let _ = commands.send(AgentCommand::ToolFailed { call_id, error }).await;
                        }
                    }
                });
            }
            None => {
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = commands.send(AgentCommand::ToolFailed {
                        call_id,
                        error: harness_protocol::tools::ToolError::ExecutionFailed,
                    }).await;
                });
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

    use super::*;

    #[tokio::test]
    async fn send_command_through_mailbox() {
        let (mut task, sender) = AgentTask::new(AgentId::new());
        sender.send(AgentCommand::Cancel).await.expect("send command");
        assert!(matches!(task.commands.recv().await, Some(AgentCommand::Cancel)));
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
}

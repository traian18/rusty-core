//! Async runtime loop for a single deterministic agent.
//!
//! The loop and effect dispatch live here; work that runs off the loop is in
//! `backend_run` and `tool_run`, child agents in `children`, and event
//! publication in `publishing`.

mod backend_run;
mod children;
mod mailbox;
mod publishing;
mod tool_run;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use harness_core::agent::Agent;
use harness_core::transcript::validate_transcript;
use harness_protocol::backend::ExecutionRequest;
use harness_protocol::commands::{AgentCommand, AgentError, AgentResult, AgentStatus};
use harness_protocol::effects::{
    AgentEffect, SessionMutation, SpawnAgentSpec, SpawnMode, ToolRequest,
};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope};
use harness_protocol::ids::{AgentId, RunId, ToolCallId};
use harness_session_store::SessionCommitter;

#[cfg(test)]
use backend_run::{forward_backend_events, BACKEND_EVENT_BUFFER};
use backend_run::{invalid_request, BackendRun};
pub use children::AgentSupervision;
use children::{spawn_refused_result, spawned_child_result, start_child_task, ToolSpawnOutcome};
use mailbox::deliver;
use tool_run::ToolRun;

use crate::agent_supervisor::AgentSupervisor;
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
    /// In-flight completion-gate evaluations, by run.
    evaluation_tokens: HashMap<RunId, CancellationToken>,
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
            evaluation_tokens: HashMap::new(),
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
        self.supervision = Some(AgentSupervision::new(supervisor, integrations));
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
        while let Some(command) = self.next_command().await {
            if !self.handle_command(command).await {
                break;
            }
        }
    }

    /// The next command to apply, or `None` once the loop must stop: every
    /// other sender is gone, the mailbox closed, or the runner was cancelled
    /// (which cancels the active run first).
    async fn next_command(&mut self) -> Option<AgentCommand> {
        loop {
            if self.task.commands_tx.strong_count() == 1 {
                return None;
            }
            if self.cancel.is_cancelled() {
                self.cancel_run().await;
                return None;
            }

            let command = tokio::select! {
                command = self.task.commands.recv() => command?,
                _ = self.cancel.cancelled() => {
                    self.cancel_run().await;
                    return None;
                },
                _ = tokio::time::sleep(Duration::from_millis(250)) => continue,
            };

            if self.cancel.is_cancelled() {
                self.cancel_run().await;
                return None;
            }
            return Some(command);
        }
    }

    /// Applies one command and its effects; whether the loop keeps going.
    async fn handle_command(&mut self, command: AgentCommand) -> bool {
        let effects = self.apply_and_publish(command);
        let finished_run = self.dispatch_effects(effects).await;
        if self.fatal_error {
            return false;
        }
        if !finished_run {
            return true;
        }
        if !self.long_lived {
            return false;
        }
        let effects = self.apply_and_publish(AgentCommand::StartNextQueuedRun);
        self.dispatch_effects(effects).await;
        true
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

    /// Runs `effects` in order; whether the run finished. A failed emit
    /// finishes it and skips the effects after it.
    async fn dispatch_effects(&mut self, effects: Vec<AgentEffect>) -> bool {
        let mut finish = false;
        for effect in effects {
            match self.dispatch_effect(effect).await {
                Dispatched::Continue => {}
                Dispatched::RunFinished => finish = true,
                Dispatched::Stop => return true,
            }
        }
        finish
    }

    async fn dispatch_effect(&mut self, effect: AgentEffect) -> Dispatched {
        match effect {
            AgentEffect::Emit { event } => {
                if !self.emit(event).await {
                    return Dispatched::Stop;
                }
            }
            AgentEffect::FinishRun { result } => {
                self.record_final_result(result);
                return Dispatched::RunFinished;
            }
            AgentEffect::ExecuteBackend { request } => self.execute_backend(request).await,
            AgentEffect::ExecuteTool { request } => self.execute_tool(request).await,
            AgentEffect::SpawnAgent { spec } => {
                if let Err(message) = self.spawn_agent(spec).await {
                    tracing::warn!(%message, "SpawnAgent effect did not spawn a child");
                }
            }
            AgentEffect::RequestPermission { request } => {
                // The preceding PermissionRequested event is the single
                // host-facing delivery path. This effect deliberately
                // performs no second publication: the runner remains in
                // WaitingForPermission until the matching
                // PermissionResolved command arrives.
                debug_assert!(self
                    .agent
                    .state
                    .pending_permissions
                    .contains_key(&request.id));
            }
            AgentEffect::Persist { mutation } => self.persist(mutation).await,
            AgentEffect::EvaluateCompletion { request } => self.evaluate_completion(request),
            cancel @ (AgentEffect::CancelBackend { .. }
            | AgentEffect::CancelTool { .. }
            | AgentEffect::CancelChild { .. }
            | AgentEffect::CancelEvaluation { .. }) => self.dispatch_cancel(cancel).await,
        }
        Dispatched::Continue
    }

    /// The `Cancel*` effects [`Self::dispatch_effect`] routes here.
    async fn dispatch_cancel(&mut self, effect: AgentEffect) {
        match effect {
            AgentEffect::CancelBackend { run_id } => {
                cancel_token(&mut self.backend_tokens, &run_id)
            }
            AgentEffect::CancelTool { call_id } => self.cancel_tool(call_id),
            AgentEffect::CancelEvaluation { run_id } => {
                cancel_token(&mut self.evaluation_tokens, &run_id)
            }
            AgentEffect::CancelChild { agent_id } => self.cancel_child(agent_id).await,
            _ => unreachable!("dispatch_effect routes only cancel effects here"),
        }
    }

    fn cancel_tool(&mut self, call_id: ToolCallId) {
        cancel_token(&mut self.tool_tokens, &call_id);
    }

    fn record_final_result(&mut self, result: AgentResult) {
        self.final_result = Some(match self.agent.state.status {
            AgentStatus::Failed => {
                Err(self
                    .agent
                    .state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| AgentError {
                        message: result.summary.clone(),
                        code: "FAILED".into(),
                        details: None,
                    }))
            }
            _ => Ok(result),
        });
    }

    async fn cancel_child(&self, agent_id: AgentId) {
        let Some(supervision) = &self.supervision else {
            tracing::warn!(?agent_id, "CancelChild requested without a supervisor");
            return;
        };
        if let Err(error) = supervision.supervisor.cancel_child(agent_id).await {
            tracing::warn!(?agent_id, %error, "failed to cancel child agent");
        }
    }

    async fn spawn_agent(&mut self, spec: SpawnAgentSpec) -> Result<ToolSpawnOutcome, String> {
        let Some(supervision) = self.supervision.clone() else {
            return Err("SpawnAgent requested without a supervisor".to_string());
        };
        let awaiting = matches!(spec.mode, SpawnMode::AwaitResult);
        let task = spec.task.clone();
        let child_id = supervision
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
            .map_err(|error| error.to_string())?;

        // M5: kick off the child's initial task, if one was given — before
        // anything might block on `await_child` below, so an `AwaitResult`
        // spawn doesn't wait on a child that was never told to do anything.
        // Pre-M5 callers that leave `task: None` keep the original behavior:
        // an idle child, started by whatever Rust orchestration code sends
        // it a command later.
        if let Some(task) = task {
            start_child_task(supervision.supervisor.as_ref(), child_id, task).await;
        }

        let effects = self.apply_and_publish(AgentCommand::ChildSpawned {
            agent_id: child_id,
            awaiting,
        });
        Box::pin(self.dispatch_effects(effects)).await;
        let awaited = if awaiting {
            Some(self.await_child(&supervision, child_id).await)
        } else {
            None
        };
        Ok(ToolSpawnOutcome { child_id, awaited })
    }

    /// Waits for an `AwaitResult` child; a failed wait fails the child in
    /// the core too.
    async fn await_child(
        &mut self,
        supervision: &AgentSupervision,
        child_id: AgentId,
    ) -> Result<AgentResult, String> {
        let error = match supervision.supervisor.await_child(child_id).await {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let effects = self.apply_and_publish(AgentCommand::ChildFailed {
            agent_id: child_id,
            error: AgentError {
                message: error.to_string(),
                code: "CHILD_AWAIT_FAILED".into(),
                details: None,
            },
        });
        Box::pin(self.dispatch_effects(effects)).await;
        Err(error.to_string())
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
            tracing::error!(
                ?error,
                "refusing to persist mutation for invalid transcript"
            );
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
    /// Repair the outgoing history, then validate it before dispatch. The
    /// recorded transcript stays untouched. The request then runs off the
    /// loop (see [`BackendRun`]).
    async fn execute_backend(&mut self, mut request: ExecutionRequest) {
        request.messages = harness_protocol::messages::repair_tool_history(request.messages);
        if let Err(message) = self.check_backend_request(&request) {
            let refused = invalid_request(request.run_id, request.request_id, message);
            deliver(&self.task.commands_tx, refused).await;
            return;
        }

        let traced = request.params.provider_options["rusty"]["trace_model_requests"] == true;
        if traced {
            let prepared = AgentEvent::ModelRequestPrepared {
                request: Box::new(request.clone()),
            };
            if !self.emit(prepared).await {
                return;
            }
        }
        let token = self.cancel.child_token();
        self.backend_tokens.insert(request.run_id, token.clone());
        BackendRun {
            backend: self.backend.clone(),
            scheduler: self.scheduler.clone(),
            commands: self.task.commands_tx.clone(),
            runner_cancel: self.cancel.clone(),
            token,
        }
        .start(request);
    }

    /// Why `request` must not reach the backend, if it must not.
    fn check_backend_request(&self, request: &ExecutionRequest) -> Result<(), String> {
        // Covers direct runtime construction, restore and child backends too.
        if self.backend.capabilities().backend_managed_tools {
            return Err("Integration must return tool calls to the harness; backend-managed execution is disabled".into());
        }
        validate_transcript(&request.messages).map_err(|error| {
            tracing::error!(
                ?error,
                "refusing to dispatch backend request with invalid transcript"
            );
            format!("invalid transcript: {error}")
        })
    }

    /// Executes a tool call and reports the result to the agent's mailbox.
    async fn execute_tool(&mut self, request: ToolRequest) {
        let call_id = request.call.id;
        let name = request.call.name;
        let arguments = request.call.arguments;

        if !self.tool_call_permitted(call_id, &name, &arguments) {
            let error = harness_protocol::tools::ToolError::PermissionDenied;
            deliver(
                &self.task.commands_tx,
                AgentCommand::ToolFailed { call_id, error },
            )
            .await;
            return;
        }

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
        let run = ToolRun {
            call_id,
            executor: self.tool_registry.get_executor(&name),
            name,
            arguments,
            scheduler: self.scheduler.clone(),
            token,
            commands: self.task.commands_tx.clone(),
            session_id: self.agent.session_id.to_string(),
        };
        tokio::spawn(run.run());
    }

    /// Recheck the actual capability before every route, including spawn.
    /// `request.permission` is an effect field, not an authorization grant.
    fn tool_call_permitted(
        &self,
        call_id: ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        use crate::permissions::{PermissionOutcome, PermissionPolicy};
        match PermissionPolicy.evaluate(&self.agent.capabilities, name) {
            PermissionOutcome::Allow => true,
            PermissionOutcome::Denied(_) => false,
            PermissionOutcome::RequiresApproval(_) => self.call_approved(call_id, name, arguments),
        }
    }

    /// The call needing approval is the one that was pending, and no
    /// permission request for it is still open.
    fn call_approved(
        &self,
        call_id: ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        let state = &self.agent.state;
        let is_pending_call = state.pending_tools.get(&call_id).is_some_and(|pending| {
            pending.call.name == name && pending.call.arguments == *arguments
        });
        is_pending_call && !state.pending_permissions.values().any(|id| *id == call_id)
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
        let result = match crate::spawn_tool::build_spawn_spec(arguments, &self.agent) {
            Ok((mut spec, warnings)) => {
                spec.origin_tool_call_id = Some(call_id);
                match self.spawn_agent(spec).await {
                    Ok(outcome) => spawned_child_result(call_id, &outcome, &warnings),
                    Err(message) => {
                        tracing::warn!(%message, "agent.spawn tool call could not spawn a child");
                        spawn_refused_result(call_id, message)
                    }
                }
            }
            Err(message) => spawn_refused_result(call_id, message),
        };
        let effects = self.apply_and_publish(AgentCommand::ToolCompleted { call_id, result });
        Box::pin(self.dispatch_effects(effects)).await;
    }

    /// Run the completion gate's evaluators off the mailbox loop and report
    /// the verdicts back as `CompletionEvaluated`, like a tool result.
    fn evaluate_completion(
        &mut self,
        request: harness_protocol::effects::CompletionEvaluationRequest,
    ) {
        let token = self.cancel.child_token();
        if let Some(previous) = self.evaluation_tokens.insert(request.run_id, token.clone()) {
            previous.cancel();
        }
        let context = crate::completion_gate::GateContext {
            backend: self.backend.clone(),
            tool_registry: self.tool_registry.clone(),
            workspace: self.workspace.clone(),
            commands_trusted: self.agent.state.behavior.commands_trusted,
            tool_permissions: self
                .agent
                .capabilities
                .tools
                .tools
                .values()
                .filter(|capability| capability.policy.enabled)
                .map(|capability| {
                    (
                        capability.descriptor.name.clone(),
                        capability.policy.permission.clone(),
                    )
                })
                .collect(),
            params: self
                .agent
                .state
                .behavior
                .profile
                .execution_params(&self.agent.state.execution_params),
            session_id: self.agent.session_id.to_string(),
        };
        let commands = self.task.commands_tx.clone();
        tokio::spawn(async move {
            let (verdicts, usage) =
                crate::completion_gate::evaluate(&context, &request, &token).await;
            // A cancelled evaluation still spent its requests; report them
            // so usage stays truthful (the core ignores the stale verdicts).
            if token.is_cancelled() && usage.is_empty() {
                return;
            }
            let evaluated = AgentCommand::CompletionEvaluated {
                run_id: request.run_id,
                attempt: request.attempt,
                verdicts,
                usage,
            };
            deliver(&commands, evaluated).await;
        });
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.agent.state.status,
            AgentStatus::Cancelled | AgentStatus::Completed | AgentStatus::Failed
        )
    }
}

/// How dispatching one effect leaves the run.
enum Dispatched {
    Continue,
    /// A `FinishRun` effect completed the run.
    RunFinished,
    /// An event could not be committed; the effects after it are skipped.
    Stop,
}

/// Cancel and forget the in-flight work `key` names, if any.
fn cancel_token<K: std::hash::Hash + Eq>(tokens: &mut HashMap<K, CancellationToken>, key: &K) {
    if let Some(token) = tokens.remove(key) {
        token.cancel();
    }
}

#[cfg(test)]
mod tests;

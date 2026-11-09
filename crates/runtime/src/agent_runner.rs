//! Async runtime loop for a single deterministic agent.
//!
//! The loop and effect dispatch live here; work that runs off the loop is in
//! `backend_run` and `tool_run`, child agents in `children`, and event
//! publication in `publishing`.

mod backend_request;
mod backend_run;
mod children;
mod completion;
mod dispatch;
mod mailbox;
mod observation_publisher;
mod outcome;
mod prompt_prefix;
mod publishing;
mod setup;
pub use setup::RunnerServices;
mod spawning;
mod tool_request;
mod tool_run;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rusty_agent_core::agent::Agent;

use rusty_protocol::commands::{AgentCommand, AgentError, AgentResult, AgentStatus};
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::AgentEventEnvelope;
use rusty_protocol::ids::{AgentId, RunId, ToolCallId};
use rusty_session_store::SessionCommitter;

#[cfg(test)]
use backend_run::{forward_backend_events, BACKEND_EVENT_BUFFER};
pub use children::AgentSupervision;

use crate::scheduler::Scheduler;
use crate::session_runtime::{AgentProjectionTable, LiveStateTable};
use crate::traits::{
    EventSink, ExecutionBackend, RequestObservationBindings, ToolRegistry, Workspace,
};

/// Mailbox and control handles for one agent.
pub struct AgentTask {
    pub id: AgentId,
    pub commands: mpsc::Receiver<AgentCommand>,
    /// Extra sender kept so background tasks can enqueue commands.
    pub commands_tx: mpsc::Sender<AgentCommand>,
    pub events: broadcast::Sender<AgentEventEnvelope>,
    pub cancel: CancellationToken,
    observation_tx: mpsc::UnboundedSender<observation_publisher::Publication>,
    observation_rx: mpsc::UnboundedReceiver<observation_publisher::Publication>,
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
        let (observation_tx, observation_rx) = mpsc::unbounded_channel();
        let task = Self {
            id,
            commands,
            commands_tx: commands_tx.clone(),
            events,
            cancel: CancellationToken::new(),
            observation_tx,
            observation_rx,
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
    session_store: Option<Arc<dyn rusty_session_store::SessionStore>>,
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
    prompt_prefix: prompt_prefix::PromptPrefixWatch,
    /// Agent-scoped ordering for qualifying context request starts.
    pub(crate) request_start_order: u64,
    /// Session-owned request bindings consumed by context wrappers.
    pub(crate) observation_bindings: RequestObservationBindings,
    /// Where committed lifecycle events are kept for the open session's recovery.
    lifecycle_retention: Option<crate::session_runtime::LifecycleRetention>,
}

impl AgentRunner {
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
                publication = self.task.observation_rx.recv() => {
                    if let Some(publication) = publication {
                        let published = self
                            .emit_observation(publication.event, publication.run_id)
                            .await;
                        let _ = publication.result.send(published);
                    }
                    continue;
                },
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

    fn is_terminal(&self) -> bool {
        matches!(
            self.agent.state.status,
            AgentStatus::Cancelled | AgentStatus::Completed | AgentStatus::Failed
        )
    }
}

#[cfg(test)]
mod tests;

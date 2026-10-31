//! Building a runner: the optional collaborators it can be given before it runs.

use std::collections::HashMap;
use std::sync::Arc;

use rusty_agent_core::agent::Agent;
use tokio_util::sync::CancellationToken;

use rusty_protocol::commands::{AgentError, AgentResult};
use rusty_session_store::SessionCommitter;

use super::{AgentRunner, AgentSupervision, AgentTask};
use crate::agent_supervisor::AgentSupervisor;
use crate::integration::IntegrationRegistry;
use crate::scheduler::Scheduler;
use crate::session_runtime::{AgentProjectionTable, LiveStateTable};
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

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
            prompt_prefix: Default::default(),
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
    pub fn with_session_store(mut self, store: Arc<dyn rusty_session_store::SessionStore>) -> Self {
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
}

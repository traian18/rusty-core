//! How a session's tasks start and end: its root task, failure, extra agent runners, cancel and shutdown, checkpoints.

use super::{SessionRuntime, SessionStatus};
use crate::locks::MutexExt;
use tokio::sync::mpsc;

use rusty_agent_core::agent::Agent;
use rusty_protocol::commands::AgentCommand;

use rusty_session_store::StoreError;

use super::runner_spawn::{spawn_runner, RunnerSpawnArgs};

impl SessionRuntime {
    /// Take the root task's [`JoinHandle`](tokio::task::JoinHandle), if it has not been taken already.
    ///
    /// This allows a supervisor (e.g. [`SessionManager`](crate::session_manager::SessionManager)) to await the root
    /// task and detect panics or unexpected terminations. The handle can only
    /// be taken once; subsequent calls return `None`.
    pub fn take_root_task_handle(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.root_task_handle.lock_or_recover().take()
    }

    /// Mark the session as failed with the given error message.
    ///
    /// This transitions the session's status to [`SessionStatus::Failed`] and
    /// stores the error description. It is called by the supervisor task when
    /// the root task's `JoinHandle` produces a [`JoinError`](tokio::task::JoinError).
    pub fn mark_failed(&self, error_msg: String) {
        let mut state = self.state.lock_or_recover();
        state.status = SessionStatus::Failed;
        state.error = Some(error_msg);
    }

    /// Spawn an agent runner, register its event stream with the session bus,
    /// and return the runner's command sender.
    ///
    /// This is the common lifecycle path for additional agents.  The root
    /// runner is created inline during construction so its sender can be
    /// retained by the runtime; later agents use this method.
    pub fn spawn_agent_runner(&self, agent: Agent) -> mpsc::Sender<AgentCommand> {
        let is_root = agent.parent_id.is_none();
        let (command_tx, _join) = spawn_runner(RunnerSpawnArgs {
            agent,
            is_root,
            backend: self.default_backend.clone(),
            tool_registry: self.tool_registry.clone(),
            workspace: self.workspace.clone(),
            event_sink: self.event_sink.clone(),
            scheduler: self.scheduler.clone(),
            cancellation: self.cancellation.clone(),
            agent_supervisor: self.agent_supervisor.clone(),
            integrations: self.integrations.clone(),
            live_state: self.live_state.clone(),
            projection: self.projection.clone(),
            event_bus: self.event_bus.clone(),
            session_store: self.session_store.clone(),
            committer: self.committer.clone(),
        });
        command_tx
    }

    /// Cancel the entire session.
    ///
    /// Triggers the hierarchical cancellation root (stopping all agent
    /// runners and spawned backend/tool tasks) and sends
    /// [`AgentCommand::Cancel`] to the root agent.
    ///
    /// The session status immediately transitions to [`SessionStatus::Cancelled`]
    /// (this is an optimistic update — cancellation is deterministic once
    /// the token fires, even though the agent runner processes it
    /// asynchronously).
    ///
    /// The event bus is **not** shut down by this call — subscribers can
    /// still receive terminal events (e.g. `Completed { Cancelled }`) after
    /// cancellation returns.
    pub async fn cancel(&self) {
        self.cancellation.cancel();
        let _ = self.root_agent_tx.send(AgentCommand::Cancel).await;

        // Optimistically transition the session-level status so that
        // state_snapshot() returns Cancelled immediately.
        let mut state = self.state.lock_or_recover();
        if state.status != SessionStatus::Failed {
            state.status = SessionStatus::Cancelled;
        }
    }

    /// Shutdown the entire session, including the event bus.
    ///
    /// This is the final teardown — called by
    /// [`SessionManager::close_session`](crate::session_manager::SessionManager::close_session)
    /// and [`Drop`].  After this, no further events will be forwarded to
    /// subscribers.
    pub fn shutdown(&self) {
        self.cancellation.cancel();
        self.bus_cancel.cancel();
        self.event_bus.lifecycle_retention().close();
    }

    /// The compaction lifecycle events this open session retained after `cursor` (see
    /// [`LifecycleRetention`](super::LifecycleRetention)); unavailable once it closed.
    pub fn recover_lifecycle(&self, cursor: u64) -> super::LifecycleRecovery {
        self.event_bus.lifecycle_retention().recover(cursor)
    }

    /// Writes a truthful snapshot of the session at its last committed
    /// durable sequence (RC-302).
    ///
    /// The snapshot is built from the live per-agent projections (never a
    /// stale construction-time copy), versioned with the current
    /// [`SCHEMA_VERSION`](rusty_session_store::SCHEMA_VERSION), and
    /// recorded with the workspace identity and integration references it
    /// was taken under (RC-304). Returns `Ok(())` when no store is
    /// configured (nothing to checkpoint).
    pub async fn checkpoint(&self) -> Result<(), StoreError> {
        let Some(store) = &self.session_store else {
            return Ok(());
        };
        // The snapshot point is the store's own last committed sequence —
        // authoritative and gap-free from the committer's perspective.
        let sequence = store.current_sequence(self.session_id).await?;
        let root_agent_id = self.state.lock_or_recover().root_agent_id;
        let snapshot = super::projection::build_snapshot(
            self.session_id,
            root_agent_id,
            &self.projection,
            self.workspace.as_ref(),
            sequence,
        );
        store.save_snapshot(snapshot).await
    }
}

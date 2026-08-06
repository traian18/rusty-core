//! Session lifecycle management for the harness runtime.
//!
//! RC-300 restore validates and reduces trailing durable events before any
//! provider, tool, permission handler, or external sink is invoked.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, OwnedSemaphorePermit, RwLock};

use harness_protocol::backend::BackendReference;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_session_store::{
    migrate_snapshot, replay_snapshot, GapPolicy, HostDependencyResolver, ReplayError,
    ReplayValidator, RestoreError,
    RestorePolicy, SessionStore, SnapshotVersionError, StoreError,
};

use crate::integration::IntegrationRegistry;
use crate::restore::HostRestoreResolver;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::session_runtime::SessionRuntime;
use crate::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};

#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("session {0} not found")]
    NotFound(SessionId),
    #[error("session restore requires a configured session store")]
    RestoreNotSupported,
    #[error("session store error: {0}")]
    Store(#[from] StoreError),
    #[error("session {0} has no stored snapshot to restore from")]
    NoSnapshot(SessionId),
    #[error("stored session failed replay validation: {0}")]
    RestoreValidation(#[from] ReplayError),
    #[error("stored snapshot is not migratable: {0}")]
    SnapshotMigration(#[from] SnapshotVersionError),
    #[error("restore rejected: {0}")]
    RestoreRejected(#[from] RestoreError),
    #[error("failed to re-create backend for integration {integration}: {message}")]
    BackendCreation {
        integration: String,
        message: String,
    },
    #[error("session {session} was removed but its final checkpoint failed to persist: {error}")]
    CloseCheckpointFailed { session: SessionId, error: String },
}

pub struct SessionManager {
    sessions: RwLock<HashMap<SessionId, Arc<SessionRuntime>>>,
    session_permits: RwLock<HashMap<SessionId, OwnedSemaphorePermit>>,
    scheduler: Arc<Scheduler>,
    store: Option<Arc<dyn SessionStore>>,
    restore_policy: RestorePolicy,
}

impl SessionManager {
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::new_with_store(scheduler, None)
    }

    pub fn new_with_store(
        scheduler: Arc<Scheduler>,
        store: Option<Arc<dyn SessionStore>>,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_permits: RwLock::new(HashMap::new()),
            scheduler,
            store,
            restore_policy: RestorePolicy::RejectMissing,
        }
    }

    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_restore_policy(mut self, policy: RestorePolicy) -> Self {
        self.restore_policy = policy;
        self
    }

    pub async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Arc<SessionRuntime> {
        let session_permit = self.scheduler.acquire_session_permit().await;
        let session_id = SessionId::new();
        let runtime = Arc::new(SessionRuntime::new_with_scheduler(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
            root_toolset,
            self.scheduler.clone(),
            self.store.clone(),
        ));

        self.sessions
            .write()
            .await
            .insert(session_id, runtime.clone());
        self.session_permits
            .write()
            .await
            .insert(session_id, session_permit);

        // RC-302: establish an initial restorable checkpoint before the
        // session accepts meaningful work. This closes the event-only restore
        // gap without inventing a synthetic workspace or backend later.
        if self.store.is_some() {
            if let Err(error) = runtime.checkpoint().await {
                tracing::error!(
                    %session_id,
                    %error,
                    "failed to create initial session checkpoint"
                );
                runtime.mark_failed(format!(
                    "initial durable checkpoint failed: {error}"
                ));
            }
        }

        Self::supervise(session_id, &runtime);
        runtime
    }

    fn supervise(session_id: SessionId, runtime: &Arc<SessionRuntime>) {
        if let Some(handle) = runtime.take_root_task_handle() {
            let runtime_for_supervisor = runtime.clone();
            tokio::spawn(async move {
                if let Err(join_err) = handle.await {
                    tracing::error!(%session_id, error = %join_err, "session root task failed");
                    runtime_for_supervisor.mark_failed(join_err.to_string());
                }
            });
        }
    }

    pub async fn session_handle(&self, id: SessionId) -> Option<Arc<SessionRuntime>> {
        self.sessions.read().await.get(&id).cloned()
    }

    pub async fn subscribe(
        &self,
        id: SessionId,
    ) -> Result<broadcast::Receiver<AgentEventEnvelope>, SessionManagerError> {
        self.session_handle(id)
            .await
            .map(|runtime| runtime.event_bus.subscribe())
            .ok_or(SessionManagerError::NotFound(id))
    }

    /// Writes a final checkpoint before clean shutdown.
    ///
    /// The session is always removed from the active map and shut down, even
    /// when the final checkpoint fails to persist, so a caller cannot leak a
    /// half-closed session by retrying `close_session`. However, unlike the
    /// prior behavior of silently logging and returning `Ok(())`, a
    /// checkpoint failure is now surfaced as
    /// `SessionManagerError::CloseCheckpointFailed` so a caller cannot
    /// mistakenly believe the session's final state is durable when it is
    /// not.
    pub async fn close_session(&self, id: SessionId) -> Result<(), SessionManagerError> {
        let runtime = self
            .sessions
            .write()
            .await
            .remove(&id)
            .ok_or(SessionManagerError::NotFound(id))?;

        let checkpoint_result = runtime.checkpoint().await;
        runtime.shutdown();
        self.session_permits.write().await.remove(&id);

        if let Err(error) = checkpoint_result {
            tracing::error!(%id, %error, "failed to checkpoint session during close");
            return Err(SessionManagerError::CloseCheckpointFailed {
                session: id,
                error: error.to_string(),
            });
        }
        Ok(())
    }

    /// Restores a session without replaying external side effects.
    ///
    /// The latest snapshot is migrated, then every validated trailing event
    /// is reduced onto it before dependency resolution or backend creation.
    pub async fn restore_session(
        &self,
        id: SessionId,
        integrations: Arc<IntegrationRegistry>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        let Some(store) = &self.store else {
            tracing::warn!(%id, "restore_session called without a configured session store");
            return Err(SessionManagerError::RestoreNotSupported);
        };

        if let Some(existing) = self.session_handle(id).await {
            return Ok(existing);
        }

        let stored = store.load_session(id).await?;
        let trailing =
            ReplayValidator::new(GapPolicy::AllowEphemeralHoles).validate(&stored)?;
        let snapshot = stored
            .snapshot
            .ok_or(SessionManagerError::NoSnapshot(id))?;
        let snapshot = migrate_snapshot(snapshot)?;
        let snapshot = replay_snapshot(snapshot, &trailing)?;

        let resolver = HostRestoreResolver::new(workspace.as_ref(), integrations.clone());
        let report = resolver.resolve(id, &snapshot.metadata).await;
        harness_session_store::assess_restore(&report, self.restore_policy).inspect_err(|_error| {
            tracing::error!(
                %id,
                missing = ?report.missing,
                "restore refused: host dependencies could not be resolved"
            );
        })?;

        let mut backends: HashMap<String, Arc<dyn ExecutionBackend>> = HashMap::new();
        for agent in &snapshot.agents {
            let reference = &agent.backend.reference;
            let key = backend_reference_key(reference);
            if backends.contains_key(&key) {
                continue;
            }

            let persisted_id = reference.integration.to_string();
            let integration_id = match integrations.get(&persisted_id) {
                Ok(Some(_)) => persisted_id,
                Ok(None) => integrations
                    .id_for_descriptor_name(&agent.backend.descriptor.name)
                    .map_err(|error| SessionManagerError::BackendCreation {
                        integration: persisted_id.clone(),
                        message: error.to_string(),
                    })?
                    .ok_or_else(|| SessionManagerError::BackendCreation {
                        integration: persisted_id.clone(),
                        message: format!(
                            "no registered integration matches backend {}",
                            agent.backend.descriptor.name
                        ),
                    })?,
                Err(error) => {
                    return Err(SessionManagerError::BackendCreation {
                        integration: persisted_id,
                        message: error.to_string(),
                    });
                }
            };

            let backend = integrations
                .create(&integration_id, agent.backend_config.clone())
                .await
                .map_err(|error| SessionManagerError::BackendCreation {
                    integration: integration_id.clone(),
                    message: error.to_string(),
                })?;
            backends.insert(key, backend);
        }

        let root = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == snapshot.root_agent_id)
            .ok_or(SessionManagerError::NoSnapshot(id))?;
        let root_key = backend_reference_key(&root.backend.reference);
        let default_backend = backends.remove(&root_key).ok_or_else(|| {
            SessionManagerError::BackendCreation {
                integration: root.backend.reference.integration.to_string(),
                message: "root agent's backend was not re-created".into(),
            }
        })?;

        let session_permit = self.scheduler.acquire_session_permit().await;
        let runtime = Arc::new(SessionRuntime::from_stored(
            id,
            snapshot.agents,
            default_backend,
            tool_registry,
            workspace,
            event_sink,
            self.scheduler.clone(),
            integrations,
            Some(store.clone()),
        ));

        self.sessions.write().await.insert(id, runtime.clone());
        self.session_permits
            .write()
            .await
            .insert(id, session_permit);
        Self::supervise(id, &runtime);
        Ok(runtime)
    }

    pub async fn active_session_ids(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().copied().collect()
    }
}

fn backend_reference_key(reference: &BackendReference) -> String {
    format!("{}::{}", reference.integration, reference.configuration)
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(Arc::new(Scheduler::new(SchedulerConfig::default())))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harness_protocol::backend::{ExecutionResult};
    use harness_protocol::events::AgentEventEnvelope;
    use harness_protocol::ids::RequestId;
    use harness_protocol::usage::{Cost, ModelUsage};
    use harness_session_store::testing::MemoryStore;

    use crate::session_runtime::SessionStatus;
    use crate::testing::{FakeBackend, FakeToolRegistry};
    use crate::workspace::FakeWorkspace;

    use super::*;

    struct NoopSink;

    impl EventSink for NoopSink {
        fn send(&self, _envelope: AgentEventEnvelope) {}
    }

    fn noop_backend() -> Arc<FakeBackend> {
        let request_id = RequestId::new();
        Arc::new(FakeBackend::new().with_result(ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        }))
    }

    #[tokio::test]
    async fn initial_checkpoint_failure_is_a_truthful_session_failure() {
        let store = Arc::new(MemoryStore::new());
        store.set_fail_snapshots(true);
        let manager = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store),
        );
        let runtime = manager
            .create_session(
                noop_backend(),
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
                AgentToolset {
                    tools: HashMap::new(),
                },
            )
            .await;

        let snapshot = runtime.state_snapshot();
        assert_eq!(snapshot.status, SessionStatus::Failed);
        assert!(snapshot
            .error
            .is_some_and(|error| error.contains("initial durable checkpoint failed")));
    }

    #[tokio::test]
    async fn close_session_surfaces_a_failed_final_checkpoint_instead_of_swallowing_it() {
        let store = Arc::new(MemoryStore::new());
        let manager = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );
        let runtime = manager
            .create_session(
                noop_backend(),
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
                AgentToolset {
                    tools: HashMap::new(),
                },
            )
            .await;
        assert_eq!(runtime.state_snapshot().status, SessionStatus::Idle);

        let ids = manager.active_session_ids().await;
        assert_eq!(ids.len(), 1);
        let session_id = ids[0];

        // The initial checkpoint above succeeded; now fail the store so the
        // *final* checkpoint taken during close fails.
        store.set_fail_snapshots(true);

        let result = manager.close_session(session_id).await;
        assert!(matches!(
            result,
            Err(SessionManagerError::CloseCheckpointFailed { session, .. }) if session == session_id
        ));

        // The session must still be fully removed from the active map even
        // though its final checkpoint failed, so callers cannot leak it by
        // retrying close.
        assert!(manager.session_handle(session_id).await.is_none());
        assert!(matches!(
            manager.close_session(session_id).await,
            Err(SessionManagerError::NotFound(id)) if id == session_id
        ));
    }

    /// M2: `close_session` only *signals* cancellation (`runtime.shutdown()`
    /// cancels the token) — it does not and cannot await the root agent
    /// task's actual exit, because that task's `JoinHandle` was already
    /// consumed by `SessionManager::supervise` at session-creation time. So
    /// `close_session` must (a) return promptly even while a run is still
    /// genuinely in flight, rather than hanging, and (b) the in-flight run's
    /// eventual cancellation must still land exactly once in the durable
    /// store — no lost event (the caller walking away must not silently
    /// orphan a still-running backend call) and no duplicate terminal event
    /// (the runner's own cancel-detection path and any late backend
    /// callback must not both commit a terminal transition).
    #[tokio::test]
    async fn close_session_races_an_active_run_without_losing_or_duplicating_the_terminal_event()
    {
        let store = Arc::new(MemoryStore::new());
        let manager = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );
        let backend = Arc::new(FakeBackend::new().blocking_until_cancelled());
        let runtime = manager
            .create_session(
                backend,
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
                AgentToolset {
                    tools: HashMap::new(),
                },
            )
            .await;
        let session_id = runtime.session_id;
        let root_agent_id = runtime.state_snapshot().root_agent_id;

        runtime
            .send_command(crate::session_runtime::SessionCommand::Prompt(
                harness_protocol::commands::UserInput {
                    text: "start a run that will still be in flight at close time".into(),
                    attachments: vec![],
                },
            ))
            .await
            .expect("prompt should be accepted");

        // Wait until the run is genuinely in flight (the FakeBackend is now
        // blocking on cancellation) before racing close_session against it.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if runtime.agent_live_state(root_agent_id).status
                    == harness_protocol::commands::AgentStatus::PreparingContext
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should reach PreparingContext before close races it");

        // close_session must return promptly: it cannot and does not wait
        // for the root task to actually exit.
        tokio::time::timeout(std::time::Duration::from_secs(1), manager.close_session(session_id))
            .await
            .expect("close_session must not hang waiting on the in-flight run")
            .expect("close_session should succeed (the initial checkpoint is healthy)");
        assert!(manager.session_handle(session_id).await.is_none());

        // The root task keeps running in the background after close_session
        // returns (its JoinHandle belongs to the supervisor, not to close);
        // it must still observe the cancellation and durably commit exactly
        // one terminal event, without a caller ever calling checkpoint again.
        //
        // Read `raw_records` rather than `load_session`: a terminal event
        // triggers an automatic `checkpoint_for_terminal_run` at the same
        // sequence, so the terminal event can legitimately be compacted into
        // the snapshot boundary and would then be (correctly) absent from
        // `load_session`'s trailing-events view. `raw_records` sees both the
        // event log and the snapshot, so it can't be fooled by that boundary.
        let raw = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let raw = store
                    .raw_records(session_id)
                    .await
                    .expect("session must still be loadable from the store after close");
                let has_terminal = raw.iter().any(|record| match record {
                    harness_session_store::RawRecord::Event(event) => matches!(
                        event.envelope.event,
                        harness_protocol::events::AgentEvent::Completed { .. }
                    ),
                    harness_session_store::RawRecord::Snapshot(snapshot) => snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.agent_id == root_agent_id)
                        .is_some_and(|agent| {
                            agent.status == harness_protocol::commands::AgentStatus::Cancelled
                        }),
                });
                if has_terminal {
                    return raw;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect(
            "the in-flight run's cancellation must still be durably committed \
             after close_session returns, not silently dropped",
        );

        let terminal_events = raw
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    harness_session_store::RawRecord::Event(event)
                        if matches!(
                            event.envelope.event,
                            harness_protocol::events::AgentEvent::Completed { .. }
                        )
                )
            })
            .count();
        assert_eq!(
            terminal_events, 1,
            "close racing an in-flight run must commit exactly one terminal event, \
             never zero (lost) and never more than one (duplicated)"
        );

        // No two durable events may share a session sequence number — the
        // race must not have corrupted ordering either.
        let mut sequences: Vec<u64> = raw
            .iter()
            .filter_map(|record| match record {
                harness_session_store::RawRecord::Event(event) => event.session_sequence,
                harness_session_store::RawRecord::Snapshot(_) => None,
            })
            .collect();
        let before_dedup = sequences.len();
        sequences.sort_unstable();
        sequences.dedup();
        assert_eq!(
            sequences.len(),
            before_dedup,
            "no durable event may duplicate another event's session sequence"
        );
    }
}

//! Session lifecycle management for the harness runtime.
//!
//! RC-300 restore validates and reduces trailing durable events before any
//! provider, tool, permission handler, or external sink is invoked.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, OwnedSemaphorePermit, RwLock};

use harness_protocol::backend::BackendReference;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_session_store::{
    migrate_snapshot, replay_snapshot, GapPolicy, HostDependencyResolver, ReplayError,
    ReplayValidator, RestoreError, RestorePolicy, SessionStore, SnapshotVersionError, StoreError,
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
    /// E1: no session-permit slot became available within the scheduler's
    /// configured admission timeout — a typed, fail-fast rejection instead
    /// of blocking the caller indefinitely at capacity.
    #[error("session admission rejected: {0}")]
    AtCapacity(#[from] crate::scheduler::CapacityError),
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

    pub fn new_with_store(scheduler: Arc<Scheduler>, store: Option<Arc<dyn SessionStore>>) -> Self {
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

    /// The shared scheduler this manager's sessions acquire permits from —
    /// exposed for M6 diagnostics (`Scheduler::snapshot`); not otherwise
    /// meant for callers to acquire permits through directly.
    pub fn scheduler(&self) -> Arc<Scheduler> {
        self.scheduler.clone()
    }

    /// Number of sessions currently tracked as active by this manager.
    pub async fn active_session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// E1: bounded-wait admission — waits up to the scheduler's configured
    /// `admission_timeout` for a session slot, then returns
    /// `SessionManagerError::AtCapacity` instead of blocking the caller
    /// indefinitely once `max_active_sessions` is reached. Callers that
    /// truly want to wait forever (mostly tests) should acquire a permit
    /// themselves via `Scheduler::acquire_session_permit` first.
    pub async fn create_session(
        &self,
        backend: Arc<dyn ExecutionBackend>,
        tool_registry: Arc<dyn ToolRegistry>,
        workspace: Arc<dyn Workspace>,
        event_sink: Arc<dyn EventSink>,
        root_toolset: AgentToolset,
    ) -> Result<Arc<SessionRuntime>, SessionManagerError> {
        let session_permit = self.scheduler.try_acquire_session_permit().await?;
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
                runtime.mark_failed(format!("initial durable checkpoint failed: {error}"));
            }
        }

        Self::supervise(session_id, &runtime);
        Ok(runtime)
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

    /// E3: closes every currently-active session, each through the exact
    /// same `close_session` path (checkpoint-then-shutdown, exactly once,
    /// never silently skipped) — the drain step the daemon's shutdown
    /// listener previously had nothing to call. Bounded by `grace_period`
    /// overall: sessions still open when the deadline passes are logged and
    /// left as-is rather than letting one stuck session hang the whole
    /// shutdown indefinitely. Returns the ids of sessions that failed to
    /// close cleanly (checkpoint failure) or didn't finish before the grace
    /// period — both cases the caller should treat as "not confirmed
    /// durable," not as "lost silently," since `close_session` itself
    /// already guarantees no *duplicate* or *lost* terminal event even on
    /// checkpoint failure.
    pub async fn close_all_sessions(&self, grace_period: Duration) -> Vec<SessionId> {
        let ids: Vec<SessionId> = self.sessions.read().await.keys().copied().collect();
        if ids.is_empty() {
            return Vec::new();
        }

        let closes = ids.iter().map(|&id| {
            let manager_self = self;
            async move {
                match tokio::time::timeout(grace_period, manager_self.close_session(id)).await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => {
                        tracing::error!(%id, %error, "session did not close cleanly during shutdown drain");
                        Some(id)
                    }
                    Err(_) => {
                        tracing::error!(
                            %id,
                            ?grace_period,
                            "session did not finish closing within the shutdown grace period"
                        );
                        Some(id)
                    }
                }
            }
        });

        futures::future::join_all(closes)
            .await
            .into_iter()
            .flatten()
            .collect()
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
        let trailing = ReplayValidator::new(GapPolicy::AllowEphemeralHoles).validate(&stored)?;
        let snapshot = stored.snapshot.ok_or(SessionManagerError::NoSnapshot(id))?;
        let snapshot = migrate_snapshot(snapshot)?;
        let snapshot = replay_snapshot(snapshot, &trailing)?;

        let resolver = HostRestoreResolver::new(workspace.as_ref(), integrations.clone());
        let report = resolver.resolve(id, &snapshot.metadata).await;
        harness_session_store::assess_restore(&report, self.restore_policy).inspect_err(
            |_error| {
                tracing::error!(
                    %id,
                    missing = ?report.missing,
                    "restore refused: host dependencies could not be resolved"
                );
            },
        )?;

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
        let default_backend =
            backends
                .remove(&root_key)
                .ok_or_else(|| SessionManagerError::BackendCreation {
                    integration: root.backend.reference.integration.to_string(),
                    message: "root agent's backend was not re-created".into(),
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use harness_protocol::backend::{
        BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
        ExecutionResult,
    };
    use harness_protocol::events::AgentEventEnvelope;
    use harness_protocol::ids::{BackendId, RequestId};
    use harness_protocol::usage::{Cost, ModelUsage};
    use harness_session_store::testing::{FaultInjectingStore, MemoryStore};

    use crate::integration::IntegrationFactory;
    use crate::session_runtime::SessionStatus;
    use crate::testing::{FakeBackend, FakeToolRegistry};
    use crate::traits::ExecutionBackend;
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
            .await
            .expect("create_session should succeed under default (non-exhausted) capacity");

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
            .await
            .expect("create_session should succeed under default (non-exhausted) capacity");
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
    async fn close_session_races_an_active_run_without_losing_or_duplicating_the_terminal_event() {
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
            .await
            .expect("create_session should succeed under default (non-exhausted) capacity");
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
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            manager.close_session(session_id),
        )
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

    /// E3: the daemon-shutdown-drain scenario, exercised at the
    /// `SessionManager` level (the actual logic `apps/harnessd/src/main.rs`
    /// calls into) rather than by faking the whole binary. Two sessions —
    /// one idle, one with a genuinely in-flight run — must both end up
    /// durably checkpointed after one `close_all_sessions` call, and the
    /// manager must report zero active sessions afterward.
    #[tokio::test]
    async fn close_all_sessions_drains_every_active_session_including_one_mid_run() {
        let store = Arc::new(MemoryStore::new());
        let manager = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );

        // Session A: idle, never sent a prompt.
        let idle_runtime = manager
            .create_session(
                Arc::new(FakeBackend::new()),
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
                AgentToolset {
                    tools: HashMap::new(),
                },
            )
            .await
            .expect("create_session should succeed");
        let idle_id = idle_runtime.session_id;

        // Session B: a run genuinely in flight at drain time.
        let busy_runtime = manager
            .create_session(
                Arc::new(FakeBackend::new().blocking_until_cancelled()),
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
                AgentToolset {
                    tools: HashMap::new(),
                },
            )
            .await
            .expect("create_session should succeed");
        let busy_id = busy_runtime.session_id;
        let busy_root_agent_id = busy_runtime.state_snapshot().root_agent_id;
        busy_runtime
            .send_command(crate::session_runtime::SessionCommand::Prompt(
                harness_protocol::commands::UserInput {
                    text: "still running when shutdown drains this session".into(),
                    attachments: vec![],
                },
            ))
            .await
            .expect("prompt should be accepted");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if busy_runtime.agent_live_state(busy_root_agent_id).status
                    == harness_protocol::commands::AgentStatus::PreparingContext
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run should reach PreparingContext before the drain races it");

        assert_eq!(manager.active_session_count().await, 2);

        // The actual E3 call: one bulk drain, bounded by a grace period —
        // must not hang on the still-running session B.
        let unclosed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            manager.close_all_sessions(std::time::Duration::from_secs(1)),
        )
        .await
        .expect("close_all_sessions must not hang past its own grace period");

        assert!(
            unclosed.is_empty(),
            "both sessions' initial checkpoints are healthy; neither close should fail: {unclosed:?}"
        );
        assert_eq!(
            manager.active_session_count().await,
            0,
            "no session may remain tracked as active after a full drain"
        );

        // Both sessions must be durably present in the store — the idle one
        // via its initial checkpoint, the busy one via its (still pending at
        // drain time) eventual terminal event, exactly like the single-
        // session close race test above proves for one session at a time.
        assert!(
            !store
                .raw_records(idle_id)
                .await
                .expect("idle session must be loadable")
                .is_empty(),
            "the idle session's initial checkpoint must be durable"
        );
        let busy_raw = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let raw = store.raw_records(busy_id).await.expect("busy session must be loadable");
                let has_terminal = raw.iter().any(|record| matches!(
                    record,
                    harness_session_store::RawRecord::Event(event)
                        if matches!(event.envelope.event, harness_protocol::events::AgentEvent::Completed { .. })
                ));
                if has_terminal {
                    return raw;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the busy session's cancellation must still be durably committed after the drain");
        let terminal_events = busy_raw
            .iter()
            .filter(|record| matches!(
                record,
                harness_session_store::RawRecord::Event(event)
                    if matches!(event.envelope.event, harness_protocol::events::AgentEvent::Completed { .. })
            ))
            .count();
        assert_eq!(
            terminal_events, 1,
            "the drained in-flight session must commit exactly one terminal event"
        );
    }

    /// M2: subscriber lag/reconnect at the `SessionManager`/store level —
    /// distinct from the transport-level `Subscribe { since_seq }` tests,
    /// which cover backlog replay through an RPC connection. This proves
    /// the underlying guarantee those transport tests build on: a live
    /// subscriber that falls behind the session event bus's bounded
    /// broadcast channel (capacity 256) and misses messages
    /// (`RecvError::Lagged`) can still recover a complete, gap-free,
    /// duplicate-free view of every *durable* event via
    /// `SessionStore::events_since` — ephemeral events (e.g. streamed text
    /// deltas) genuinely are not recoverable this way, which is the
    /// documented, intentional durability boundary (see the README's
    /// durability table), not a bug this test should paper over.
    #[tokio::test]
    async fn a_lagging_subscriber_recovers_the_full_durable_backlog_via_events_since() {
        let store = Arc::new(MemoryStore::new());
        let manager = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );

        // A backend that streams far more deltas than the event bus's
        // 256-capacity broadcast channel, so a subscriber that never reads
        // is guaranteed to lag.
        let request_id = RequestId::new();
        let mut events = Vec::new();
        for i in 0..500 {
            events.push(harness_protocol::backend::ExecutionEvent::TextDelta {
                request_id,
                delta: format!("chunk {i} "),
            });
        }
        let backend = Arc::new(FakeBackend::new().with_events(events).with_result(
            ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            },
        ));

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
            .await
            .expect("create_session should succeed");
        let session_id = runtime.session_id;
        let root_agent_id = runtime.state_snapshot().root_agent_id;

        // Subscribe, then deliberately never read while the flood of
        // deltas is produced — the realistic "client fell behind" scenario.
        let mut rx = runtime.event_bus.subscribe();

        runtime
            .send_command(crate::session_runtime::SessionCommand::Prompt(
                harness_protocol::commands::UserInput {
                    text: "flood the event bus".into(),
                    attachments: vec![],
                },
            ))
            .await
            .expect("prompt should be accepted");

        // Wait for the run to actually finish before touching the
        // subscriber at all, so the lag is unambiguous rather than racing
        // completion. FakeBackend paces each scripted event by STEP_DELAY
        // (2ms) to simulate real streaming latency, so 500 events take on
        // the order of ~1-2s — generous headroom above that here. Polling
        // live `agent_live_state` (not the store) for a recorded outcome
        // avoids the terminal-event/snapshot-compaction ambiguity the
        // close-race test above has to work around — durability is
        // verified separately below, this is purely "has the run finished."
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if runtime
                    .agent_live_state(root_agent_id)
                    .last_outcome
                    .is_some()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the flooded run should complete");

        // The subscriber must now observe a lag — proving the test actually
        // exercises the scenario it claims to, not a false-positive no-op.
        let mut observed_lag = false;
        loop {
            match rx.try_recv() {
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    observed_lag = true;
                    break;
                }
                Err(_) => break,
            }
        }
        assert!(
            observed_lag,
            "500 streamed deltas must overflow the 256-capacity event bus and lag this subscriber"
        );

        // Recovery: fetch the complete durable backlog from sequence 0 —
        // must include the full lifecycle (RunStarted...Completed) with no
        // gaps, regardless of how much the live broadcast channel dropped.
        let backlog = store
            .events_since(session_id, 0)
            .await
            .expect("events_since should succeed");
        assert!(
            backlog.iter().any(|event| matches!(
                event.envelope.event,
                harness_protocol::events::AgentEvent::RunStarted { .. }
            )),
            "durable backlog must include RunStarted"
        );
        assert!(
            backlog.iter().any(|event| matches!(
                event.envelope.event,
                harness_protocol::events::AgentEvent::Completed { .. }
            )),
            "durable backlog must include the terminal Completed event"
        );

        // No duplicate/out-of-order sequences in the recovered backlog.
        let mut sequences: Vec<u64> = backlog
            .iter()
            .filter_map(|event| event.session_sequence)
            .collect();
        let before_dedup = sequences.len();
        sequences.sort_unstable();
        sequences.dedup();
        assert_eq!(
            sequences.len(),
            before_dedup,
            "events_since must never return a duplicate sequence"
        );

        // A fresh subscription after recovery sees new work cleanly, with
        // no overlap/gap relative to what events_since already delivered.
        let last_recovered_seq = backlog
            .iter()
            .filter_map(|event| event.session_sequence)
            .max()
            .unwrap_or(0);
        let mut fresh_rx = runtime.event_bus.subscribe();
        runtime
            .send_command(crate::session_runtime::SessionCommand::Prompt(
                harness_protocol::commands::UserInput {
                    text: "a second, normal prompt after recovery".into(),
                    attachments: vec![],
                },
            ))
            .await
            .expect("second prompt should be accepted");
        let next_run_started_seq = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(envelope) = fresh_rx.recv().await {
                    if matches!(
                        envelope.event,
                        harness_protocol::events::AgentEvent::RunStarted { .. }
                    ) {
                        return envelope
                            .session_sequence
                            .expect("durable event must carry a sequence");
                    }
                }
            }
        })
        .await
        .expect("the second run's RunStarted should arrive on the fresh subscription");
        assert!(
            next_run_started_seq > last_recovered_seq,
            "the fresh subscription's events must continue strictly after the recovered backlog, \
             with no gap and no overlap (recovered up to {last_recovered_seq}, next live event at {next_run_started_seq})"
        );
    }

    // -----------------------------------------------------------------
    // Engine-level crash/restart fixture (M2 remaining item)
    // -----------------------------------------------------------------

    /// A [`FakeBackend`]-backed [`ExecutionBackend`] that counts how many
    /// times `execute` is actually invoked, so a test can assert a run was
    /// never silently replayed.
    struct CountingBackend {
        calls: Arc<AtomicUsize>,
        inner: FakeBackend,
        name: &'static str,
    }

    #[async_trait]
    impl ExecutionBackend for CountingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: BackendId::new(),
                name: self.name.to_string(),
                description: "A call-counting scripted backend used for testing.".to_string(),
                capabilities: self.capabilities(),
            }
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.inner.capabilities()
        }

        async fn execute(
            &self,
            request: ExecutionRequest,
            sink: broadcast::Sender<ExecutionEvent>,
            cancel: CancellationToken,
        ) -> Result<ExecutionResult, ExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(request, sink, cancel).await
        }
    }

    /// Re-creates a [`CountingBackend`] on demand so `restore_session` can
    /// reconstruct the root agent's backend, matched purely by descriptor
    /// name (the same fallback path a real restart takes, since the
    /// original session was never given a real registered integration id —
    /// see `SessionRuntime::new_with_scheduler`, which always mints a fresh
    /// random `IntegrationId`).
    struct CountingBackendFactory {
        id: &'static str,
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl IntegrationFactory for CountingBackendFactory {
        fn id(&self) -> &'static str {
            self.id
        }

        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: BackendId::new(),
                name: self.name.to_string(),
                description: "restart-time backend factory".to_string(),
                capabilities: BackendCapabilities::default(),
            }
        }

        async fn create(
            &self,
            _config: serde_json::Value,
        ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Arc::new(CountingBackend {
                calls: self.calls.clone(),
                inner: noop_backend_inner(),
                name: self.name,
            }))
        }
    }

    fn noop_backend_inner() -> FakeBackend {
        let request_id = RequestId::new();
        FakeBackend::new().with_result(ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".into(),
        })
    }

    /// Simulates a process crash (no graceful `close_session`) and restart:
    /// a fresh `SessionManager` restores the session from the same backing
    /// store via `restore_session`, and the fixture proves two things a
    /// commit-boundary-only test cannot:
    ///
    /// 1. **No side effects are replayed.** The original backend's call
    ///    counter stops advancing the instant the process "crashes"; restore
    ///    never invokes the original backend again, and the freshly
    ///    re-created backend's own counter stays at zero throughout restore
    ///    itself (it is only ever invoked by a genuinely new prompt sent
    ///    afterward).
    /// 2. **State is correctly reconstructed.** The restored runtime is not
    ///    just structurally present — it is immediately usable: a brand
    ///    new prompt against it drives a full run to completion, which is
    ///    only possible if the deterministic core's reconstructed
    ///    `AgentState` (status, transition sequence, message history) is
    ///    actually valid input to the state machine, not merely deserialized.
    #[tokio::test]
    async fn a_restored_session_replays_no_side_effects_and_is_immediately_usable() {
        const BACKEND_NAME: &str = "crash-restart-fake-backend";

        let backing_store = Arc::new(MemoryStore::new());
        let store: Arc<dyn SessionStore> =
            Arc::new(FaultInjectingStore::new(backing_store.clone()));

        let original_calls = Arc::new(AtomicUsize::new(0));
        let original_backend = Arc::new(CountingBackend {
            calls: original_calls.clone(),
            inner: noop_backend_inner(),
            name: BACKEND_NAME,
        });

        let manager_before_crash = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );

        let session_id = {
            let runtime = manager_before_crash
                .create_session(
                    original_backend.clone(),
                    Arc::new(FakeToolRegistry::new()),
                    Arc::new(FakeWorkspace::new()),
                    Arc::new(NoopSink),
                    AgentToolset {
                        tools: HashMap::new(),
                    },
                )
                .await
                .expect("create_session should succeed");
            let session_id = runtime.session_id;
            let root_agent_id = runtime.state_snapshot().root_agent_id;

            runtime
                .send_command(crate::session_runtime::SessionCommand::Prompt(
                    harness_protocol::commands::UserInput {
                        text: "before the crash".into(),
                        attachments: vec![],
                    },
                ))
                .await
                .expect("prompt should be accepted");

            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if runtime
                        .agent_live_state(root_agent_id)
                        .last_outcome
                        .is_some()
                    {
                        return;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the pre-crash run should complete");

            session_id
            // `runtime` (and `manager_before_crash`'s only handle to it) is
            // dropped here without ever calling `close_session` — a crash,
            // not a graceful shutdown. Whatever the run's own terminal
            // checkpoint already made durable is all restore has to work
            // with.
        };

        assert_eq!(
            original_calls.load(Ordering::SeqCst),
            1,
            "the pre-crash run must have called the original backend exactly once"
        );
        let sequence_at_crash = store
            .current_sequence(session_id)
            .await
            .expect("current_sequence should succeed");

        // "Restart": a brand new manager, integration registry, and backend
        // factory over the *same* backing store — nothing here is shared
        // with `manager_before_crash` except the durable data.
        let integrations = Arc::new(IntegrationRegistry::new());
        let restart_calls = Arc::new(AtomicUsize::new(0));
        integrations
            .register(Arc::new(CountingBackendFactory {
                id: "crash-restart-integration",
                name: BACKEND_NAME,
                calls: restart_calls.clone(),
            }))
            .expect("register restart-time backend factory");

        let manager_after_restart = SessionManager::new_with_store(
            Arc::new(Scheduler::new(SchedulerConfig::default())),
            Some(store.clone()),
        );

        let restored = manager_after_restart
            .restore_session(
                session_id,
                integrations,
                Arc::new(FakeToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopSink),
            )
            .await
            .expect("restore_session should reconstruct the crashed session");

        // No side effects replayed: restoring never re-invokes the original
        // backend, and the freshly re-created backend was not invoked
        // either — restore only reduces stored events onto the migrated
        // snapshot, it does not re-run the agent.
        assert_eq!(
            original_calls.load(Ordering::SeqCst),
            1,
            "restore must never call the original (pre-crash) backend again"
        );
        assert_eq!(
            restart_calls.load(Ordering::SeqCst),
            0,
            "restore must not invoke the newly re-created backend either — no run is replayed"
        );

        // Restore is read-only with respect to durable history: it must not
        // fabricate or duplicate any event just by reconstructing state.
        assert_eq!(
            store
                .current_sequence(session_id)
                .await
                .expect("current_sequence should succeed"),
            sequence_at_crash,
            "restore must not append or duplicate any durable record"
        );

        // State was correctly reconstructed, not just structurally present:
        // the snapshot identifies the same session/root agent, ...
        let snapshot = restored.state_snapshot();
        assert_eq!(snapshot.session_id, session_id);
        assert_eq!(snapshot.agent_count, 1);

        // ...and the restored runtime is immediately usable — a genuinely
        // new prompt drives a full run to completion, which is only
        // possible if the reconstructed `AgentState` (status, transition
        // sequence, message history) is valid input to the deterministic
        // core, not merely deserialized bytes.
        let root_agent_id = snapshot.root_agent_id;
        restored
            .send_command(crate::session_runtime::SessionCommand::Prompt(
                harness_protocol::commands::UserInput {
                    text: "after the restart".into(),
                    attachments: vec![],
                },
            ))
            .await
            .expect("prompt after restore should be accepted");

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if restored
                    .agent_live_state(root_agent_id)
                    .last_outcome
                    .is_some()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the post-restart run should complete");

        assert_eq!(
            restart_calls.load(Ordering::SeqCst),
            1,
            "the post-restart prompt must run against the newly re-created backend exactly once"
        );
    }
}

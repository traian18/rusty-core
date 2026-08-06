//! harnessd RPC dispatcher for the transport-neutral protocol.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use harness_engine::{FsWorkspace, Harness, SessionHandle};
use harness_protocol::admission::{AdmissionResult, CommandId, MutationMetadata};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{
    MutationCommand, RpcError, RpcErrorCategory, RpcRequestBody, RpcResponseBody,
    SessionSnapshotWire, SessionStatusWire, SessionSummaryWire,
};
use harness_runtime::rpc::RpcHandler;
use harness_runtime::session_runtime::SessionStatus;

const DEDUPLICATION_WINDOW: usize = 1024;

struct AdmissionCache {
    entries: HashMap<(SessionId, CommandId), (AdmissionResult, u64)>,
    order: VecDeque<(SessionId, CommandId)>,
}

impl AdmissionCache {
    fn new() -> Self {
        Self { entries: HashMap::new(), order: VecDeque::new() }
    }

    fn get(&self, session_id: SessionId, id: CommandId) -> Option<(AdmissionResult, u64)> {
        self.entries.get(&(session_id, id)).cloned()
    }

    fn insert(&mut self, session_id: SessionId, id: CommandId, result: AdmissionResult, revision: u64) {
        let key = (session_id, id);
        if self.entries.contains_key(&key) {
            return;
        }
        self.entries.insert(key, (result, revision));
        self.order.push_back(key);
        while self.order.len() > DEDUPLICATION_WINDOW {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
    }
}

pub struct HarnessRpcHandler {
    harness: Arc<Harness>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionHandle>>>,
    revisions: Mutex<HashMap<SessionId, u64>>,
    admissions: Mutex<AdmissionCache>,
}

impl HarnessRpcHandler {
    pub fn new(harness: Arc<Harness>) -> Self {
        Self {
            harness,
            sessions: Mutex::new(HashMap::new()),
            revisions: Mutex::new(HashMap::new()),
            admissions: Mutex::new(AdmissionCache::new()),
        }
    }

    fn lookup(&self, session_id: SessionId) -> Option<Arc<SessionHandle>> {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .get(&session_id)
            .cloned()
    }

    fn error(
        code: &'static str,
        category: RpcErrorCategory,
        retryable: bool,
        message: impl Into<String>,
    ) -> RpcResponseBody {
        RpcResponseBody::Failure(RpcError::new(code, category, retryable, message))
    }

    async fn create_session(
        &self,
        workspace_root: std::path::PathBuf,
        integration: String,
        integration_config: serde_json::Value,
        toolset: harness_protocol::tools::AgentToolset,
    ) -> RpcResponseBody {
        let builder = match self.harness.session().integration(integration, integration_config) {
            Ok(builder) => builder,
            Err(error) => {
                return Self::error(
                    "integration.invalid_configuration",
                    RpcErrorCategory::Validation,
                    false,
                    error.to_string(),
                )
            }
        };
        let workspace = Arc::new(FsWorkspace::new(workspace_root));
        match builder.toolset(toolset, workspace).start().await {
            Ok(handle) => {
                let session_id = handle.session_id();
                self.sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .insert(session_id, Arc::new(handle));
                self.revisions
                    .lock()
                    .expect("revisions mutex poisoned")
                    .insert(session_id, 0);
                RpcResponseBody::SessionCreated { session_id }
            }
            Err(error) => Self::error(
                "session.create_failed",
                RpcErrorCategory::Integration,
                false,
                error.to_string(),
            ),
        }
    }

    async fn restore_session(
        &self,
        session_id: SessionId,
        workspace_root: std::path::PathBuf,
        toolset: harness_protocol::tools::AgentToolset,
    ) -> RpcResponseBody {
        if let Some(handle) = self.lookup(session_id) {
            let _ = handle;
            let session_revision = *self
                .revisions
                .lock()
                .expect("revisions mutex poisoned")
                .get(&session_id)
                .unwrap_or(&0);
            return RpcResponseBody::SessionRestored {
                session_id,
                session_revision,
            };
        }
        match self
            .harness
            .restore_session_with_toolset(
                session_id,
                toolset,
                Arc::new(FsWorkspace::new(workspace_root)),
            )
            .await
        {
            Ok(handle) => {
                self.sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .insert(session_id, Arc::new(handle));
                let revision = self
                    .harness
                    .session_store()
                    .current_sequence(session_id)
                    .await
                    .unwrap_or(0);
                self.revisions
                    .lock()
                    .expect("revisions mutex poisoned")
                    .insert(session_id, revision);
                RpcResponseBody::SessionRestored { session_id, session_revision: revision }
            }
            Err(error) => Self::error(
                "session.restore_failed",
                RpcErrorCategory::Persistence,
                false,
                error.to_string(),
            ),
        }
    }

    async fn mutate(
        &self,
        outer_session_id: Option<SessionId>,
        metadata: MutationMetadata,
        command: MutationCommand,
    ) -> RpcResponseBody {
        if outer_session_id != Some(metadata.session_id) {
            return Self::error(
                "mutation.session_mismatch",
                RpcErrorCategory::Validation,
                false,
                "request session_id must match mutation metadata session_id",
            );
        }

        if let Some((original, revision)) = self
            .admissions
            .lock()
            .expect("admissions mutex poisoned")
            .get(metadata.session_id, metadata.command_id)
        {
            return RpcResponseBody::Admission {
                metadata,
                result: AdmissionResult::Duplicate {
                    original: Box::new(original),
                },
                session_revision: revision,
            };
        }

        let session_id = metadata.session_id;
        let current_revision = *self
            .revisions
            .lock()
            .expect("revisions mutex poisoned")
            .get(&session_id)
            .unwrap_or(&0);
        if metadata
            .expected_session_revision
            .is_some_and(|expected| expected != current_revision)
        {
            return RpcResponseBody::Admission {
                metadata,
                result: AdmissionResult::RejectedConflict {
                    current_session_revision: current_revision,
                },
                session_revision: current_revision,
            };
        }

        let Some(handle) = self.lookup(session_id) else {
            return Self::error(
                "session.not_open",
                RpcErrorCategory::NotFound,
                false,
                "session is not open; restore it before sending mutations",
            );
        };

        let is_close = matches!(&command, MutationCommand::CloseSession);
        let operation = match command {
            MutationCommand::Prompt(input) => handle.send_input(input).await,
            MutationCommand::Steer(input) => handle.steer_input(input).await,
            MutationCommand::FollowUp(input) => handle.follow_up_input(input).await,
            MutationCommand::Cancel => handle.cancel().await,
            MutationCommand::ResolvePermission { id, decision } => {
                handle.resolve_permission(id, decision).await
            }
            MutationCommand::CloseSession => handle.close().await,
        };

        let result = match operation {
            Ok(()) => AdmissionResult::Accepted,
            Err(error) => AdmissionResult::RejectedInvalidState {
                reason: error.to_string(),
            },
        };
        let accepted = matches!(result, AdmissionResult::Accepted);
        let revision = if accepted {
            current_revision.saturating_add(1)
        } else {
            current_revision
        };

        if accepted {
            self.revisions
                .lock()
                .expect("revisions mutex poisoned")
                .insert(session_id, revision);
            if is_close {
                self.sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .remove(&session_id);
            }
        }
        self.admissions
            .lock()
            .expect("admissions mutex poisoned")
            .insert(session_id, metadata.command_id, result.clone(), revision);

        RpcResponseBody::Admission {
            metadata,
            result,
            session_revision: revision,
        }
    }
}

fn wire_status(status: SessionStatus) -> SessionStatusWire {
    match status {
        SessionStatus::Idle => SessionStatusWire::Idle,
        SessionStatus::Running => SessionStatusWire::Running,
        SessionStatus::Completed => SessionStatusWire::Completed,
        SessionStatus::Cancelled => SessionStatusWire::Cancelled,
        SessionStatus::Failed => SessionStatusWire::Failed,
    }
}

fn wire_snapshot(snapshot: harness_runtime::session_client::SessionSnapshot) -> SessionSnapshotWire {
    SessionSnapshotWire {
        session_id: snapshot.session_id,
        status: wire_status(snapshot.status),
        root_agent_id: snapshot.root_agent_id,
        root_agent_status: snapshot.root_agent_status,
        usage: snapshot.usage,
        timestamp: snapshot.timestamp,
    }
}

#[async_trait]
impl RpcHandler for HarnessRpcHandler {
    async fn handle(&self, session_id: Option<SessionId>, body: RpcRequestBody) -> RpcResponseBody {
        match body {
            RpcRequestBody::Hello { .. } => Self::error(
                "protocol.invalid_dispatch",
                RpcErrorCategory::Protocol,
                false,
                "Hello must be handled by the transport",
            ),
            RpcRequestBody::CreateSession {
                workspace_root,
                integration,
                integration_config,
                toolset,
            } => {
                self.create_session(workspace_root, integration, integration_config, toolset)
                    .await
            }
            RpcRequestBody::Mutate { metadata, command } => {
                self.mutate(session_id, metadata, command).await
            }
            RpcRequestBody::ListSessions => match self.harness.list_sessions().await {
                Ok(sessions) => RpcResponseBody::SessionsListed {
                    sessions: sessions
                        .into_iter()
                        .map(|summary| SessionSummaryWire {
                            session_id: summary.session_id,
                            title: summary.title,
                            backend_name: summary.backend_name,
                            updated_at: summary.updated_at,
                            restorable: summary.restorable,
                        })
                        .collect(),
                },
                Err(error) => Self::error(
                    "session.list_failed",
                    RpcErrorCategory::Persistence,
                    true,
                    error.to_string(),
                ),
            },
            RpcRequestBody::RestoreSession {
                session_id,
                workspace_root,
                toolset,
            } => self.restore_session(session_id, workspace_root, toolset).await,
            RpcRequestBody::Snapshot => {
                let Some(session_id) = session_id else {
                    return Self::error(
                        "request.missing_session_id",
                        RpcErrorCategory::Validation,
                        false,
                        "Snapshot requires a session_id",
                    );
                };
                match self.lookup(session_id) {
                    Some(handle) => RpcResponseBody::Snapshot(wire_snapshot(handle.snapshot())),
                    None => Self::error(
                        "session.not_open",
                        RpcErrorCategory::NotFound,
                        false,
                        "session is not open",
                    ),
                }
            }
            RpcRequestBody::Subscribe { .. } => Self::error(
                "protocol.invalid_dispatch",
                RpcErrorCategory::Protocol,
                false,
                "Subscribe must be handled by the transport",
            ),
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>> {
        self.lookup(session_id).map(|handle| handle.subscribe())
    }

    async fn events_since(&self, session_id: SessionId, since_seq: u64) -> Vec<AgentEventEnvelope> {
        if self.lookup(session_id).is_none() {
            return Vec::new();
        }
        self.harness
            .session_store()
            .events_since(session_id, since_seq)
            .await
            .map(|events| events.into_iter().map(|event| event.envelope).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::commands::UserInput;

    #[test]
    fn admission_cache_is_bounded_and_deduplicates() {
        let mut cache = AdmissionCache::new();
        let session_id = SessionId::new();
        let first = CommandId::new();
        cache.insert(session_id, first, AdmissionResult::Accepted, 1);
        assert!(cache.get(session_id, first).is_some());
        for revision in 2..=(DEDUPLICATION_WINDOW as u64 + 2) {
            cache.insert(session_id, CommandId::new(), AdmissionResult::Accepted, revision);
        }
        assert!(cache.entries.len() <= DEDUPLICATION_WINDOW);
        assert!(cache.get(session_id, first).is_none());
    }

    #[test]
    fn mutation_command_is_constructible_for_all_lifecycle_inputs() {
        let input = UserInput {
            text: "hello".into(),
            attachments: vec![],
        };
        assert!(matches!(
            MutationCommand::Prompt(input.clone()),
            MutationCommand::Prompt(_)
        ));
        assert!(matches!(
            MutationCommand::Steer(input.clone()),
            MutationCommand::Steer(_)
        ));
        assert!(matches!(
            MutationCommand::FollowUp(input),
            MutationCommand::FollowUp(_)
        ));
    }
}

//! [`RpcHandler`] implementation wrapping a [`Harness`].
//!
//! This is where "what a request means" is decided — the transport crates
//! (`harness-transport-ipc` and friends) only know how to move bytes and
//! dispatch against the `RpcHandler` trait; this module is the only place
//! that knows about `Harness`/`SessionBuilder`/`SessionHandle`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use harness_engine::{FsWorkspace, Harness, SessionHandle};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{RpcRequestBody, RpcResponseBody, SessionSnapshotWire, SessionStatusWire};
use harness_runtime::rpc::RpcHandler;
use harness_runtime::session_runtime::SessionStatus;

pub struct HarnessRpcHandler {
    harness: Arc<Harness>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionHandle>>>,
}

impl HarnessRpcHandler {
    pub fn new(harness: Arc<Harness>) -> Self {
        Self {
            harness,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn lookup(&self, session_id: SessionId) -> Option<Arc<SessionHandle>> {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .get(&session_id)
            .cloned()
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
            Err(error) => return RpcResponseBody::Error { message: error.to_string() },
        };
        let workspace = Arc::new(FsWorkspace::new(workspace_root));
        match builder.toolset(toolset, workspace).start().await {
            Ok(handle) => {
                let session_id = handle.session_id();
                self.sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .insert(session_id, Arc::new(handle));
                RpcResponseBody::SessionCreated { session_id }
            }
            Err(error) => RpcResponseBody::Error { message: error.to_string() },
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

fn wire_snapshot(
    snapshot: harness_runtime::session_client::SessionSnapshot,
) -> SessionSnapshotWire {
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
        // CreateSession is the only request that doesn't target an existing
        // session, so it's handled before the session lookup below.
        if let RpcRequestBody::CreateSession {
            workspace_root,
            integration,
            integration_config,
            toolset,
        } = body
        {
            return self
                .create_session(workspace_root, integration, integration_config, toolset)
                .await;
        }

        let Some(session_id) = session_id else {
            return RpcResponseBody::Error {
                message: "request requires a session_id".to_string(),
            };
        };
        let Some(handle) = self.lookup(session_id) else {
            return RpcResponseBody::Error {
                message: "unknown session".to_string(),
            };
        };

        match body {
            RpcRequestBody::CreateSession { .. } => unreachable!("handled above"),

            RpcRequestBody::Prompt(input) => {
                // SessionHandle::send only takes the prompt text today —
                // UserInput::attachments has no plumbing through the public
                // engine API yet. Revisit once that's added.
                match handle.send(&input.text).await {
                    Ok(()) => RpcResponseBody::Ack,
                    Err(error) => RpcResponseBody::Error { message: error.to_string() },
                }
            }

            RpcRequestBody::Cancel => match handle.cancel().await {
                Ok(()) => RpcResponseBody::Ack,
                Err(error) => RpcResponseBody::Error { message: error.to_string() },
            },

            // SessionHandle doesn't expose pause/resume yet — only
            // send/cancel/resolve_permission/subscribe/snapshot are wired
            // through the public engine API (crates/harness-engine/src/session_builder.rs).
            RpcRequestBody::Pause | RpcRequestBody::Resume => RpcResponseBody::Error {
                message: "pause/resume is not yet exposed by the session engine API".to_string(),
            },

            RpcRequestBody::ResolvePermission { id, decision } => {
                match handle.resolve_permission(id, decision).await {
                    Ok(()) => RpcResponseBody::Ack,
                    Err(error) => RpcResponseBody::Error { message: error.to_string() },
                }
            }

            RpcRequestBody::Snapshot => RpcResponseBody::Snapshot(wire_snapshot(handle.snapshot())),

            // The transport layer intercepts Subscribe before it ever reaches
            // `handle()` (see harness-transport-ipc's dispatch()), since
            // subscribing needs a long-lived receiver, not a single
            // request/response. This arm only fires if a transport forwards
            // it here anyway, which would be a bug in that transport.
            RpcRequestBody::Subscribe => RpcResponseBody::Error {
                message: "Subscribe must be handled by the transport, not dispatched to handle()"
                    .to_string(),
            },

            RpcRequestBody::CloseSession => {
                match self.harness.session_manager().close_session(session_id).await {
                    Ok(()) => {
                        self.sessions
                            .lock()
                            .expect("sessions mutex poisoned")
                            .remove(&session_id);
                        RpcResponseBody::Ack
                    }
                    Err(error) => RpcResponseBody::Error { message: error.to_string() },
                }
            }
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>> {
        self.lookup(session_id).map(|handle| handle.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use harness_integration_anthropic::AnthropicConfig;

    fn empty_toolset() -> harness_protocol::tools::AgentToolset {
        harness_protocol::tools::AgentToolset {
            tools: HashMap::new(),
        }
    }

    // `AnthropicFactory::create()` never makes a network call — it just
    // constructs a `GenericModelBackend` (see
    // crates/integrations/anthropic/src/backend.rs) — so it's safe to
    // register in tests without a real API key.
    async fn new_handler() -> (HarnessRpcHandler, tempfile::TempDir) {
        let store_dir = tempfile::tempdir().expect("tempdir");
        let harness = Harness::builder()
            .register_integration(Arc::new(harness_integration_anthropic::AnthropicFactory))
            .build()
            .await
            .expect("build harness");
        (HarnessRpcHandler::new(Arc::new(harness)), store_dir)
    }

    #[tokio::test]
    async fn create_session_then_snapshot_then_close() {
        let (handler, _store_dir) = new_handler().await;
        let workspace_dir = tempfile::tempdir().expect("workspace tempdir");

        let create = handler
            .handle(
                None,
                RpcRequestBody::CreateSession {
                    workspace_root: workspace_dir.path().to_path_buf(),
                    integration: "anthropic".to_string(),
                    integration_config: serde_json::to_value(AnthropicConfig::new("test-key"))
                        .unwrap(),
                    toolset: empty_toolset(),
                },
            )
            .await;
        let session_id = match create {
            RpcResponseBody::SessionCreated { session_id } => session_id,
            other => panic!("expected SessionCreated, got {other:?}"),
        };

        let snapshot = handler.handle(Some(session_id), RpcRequestBody::Snapshot).await;
        assert!(matches!(snapshot, RpcResponseBody::Snapshot(_)));

        let closed = handler.handle(Some(session_id), RpcRequestBody::CloseSession).await;
        assert!(matches!(closed, RpcResponseBody::Ack));

        // The session is gone from the handler's map now.
        let after_close = handler.handle(Some(session_id), RpcRequestBody::Snapshot).await;
        assert!(matches!(after_close, RpcResponseBody::Error { .. }));
    }

    #[tokio::test]
    async fn unknown_session_returns_error() {
        let (handler, _store_dir) = new_handler().await;
        let response = handler
            .handle(Some(SessionId::new()), RpcRequestBody::Snapshot)
            .await;
        assert!(matches!(response, RpcResponseBody::Error { .. }));
    }

    #[tokio::test]
    async fn request_without_session_id_returns_error() {
        let (handler, _store_dir) = new_handler().await;
        let response = handler.handle(None, RpcRequestBody::Snapshot).await;
        assert!(matches!(response, RpcResponseBody::Error { .. }));
    }
}

//! Wire-format RPC contract shared by every transport (`harness-transport-ipc`,
//! `harness-transport-websocket`, `harness-transport-stdio`).
//!
//! These types are deliberately framing-agnostic: each request/response is one
//! independently-serializable JSON value. How bytes are delimited on the wire
//! (length-prefixing, WebSocket message boundaries, newline-delimiting) is a
//! transport concern, defined in each transport crate — not a protocol
//! concern, and not defined here. This module holds only serializable data,
//! matching this crate's "no runtime or I/O policy" invariant; the behavioral
//! counterpart (`RpcHandler`, which dispatches these types against a live
//! session) lives in `harness-runtime` since it needs `async_trait` and
//! `tokio::sync::broadcast`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::commands::{PermissionDecision, UserInput};
use crate::events::AgentEventEnvelope;
use crate::ids::{AgentId, PermissionId, SessionId, Timestamp};
use crate::tools::AgentToolset;
use crate::usage::{AgentUsageSnapshot, SessionUsageSnapshot};

/// Client-assigned correlation id, echoed back on the matching [`RpcResponse`]
/// so a caller can match responses to requests on a connection carrying
/// multiple in-flight requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestCorrelationId(pub u64);

/// A request sent to a running harness daemon over any transport.
///
/// Every request carries an explicit `session_id` (absent only for
/// [`RpcRequestBody::CreateSession`]) so a single connection can address any
/// number of sessions rather than being pinned to one session for its
/// lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: RequestCorrelationId,
    pub session_id: Option<SessionId>,
    pub body: RpcRequestBody,
}

/// The set of operations a client can request.
///
/// Mirrors [`harness_runtime::session_runtime::SessionCommand`] plus the
/// session-lifecycle and read operations ([`SessionClient`]-level concerns)
/// that command alone doesn't cover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcRequestBody {
    /// Create a new session against `workspace_root`, resolving `integration`
    /// (e.g. `"anthropic"`) with the given provider-specific JSON config.
    CreateSession {
        workspace_root: PathBuf,
        integration: String,
        integration_config: serde_json::Value,
        toolset: AgentToolset,
    },
    /// Start a run with the given user input.
    Prompt(UserInput),
    /// Cancel the session's current run.
    Cancel,
    /// Pause the session.
    Pause,
    /// Resume a paused session.
    Resume,
    /// Resolve a pending permission request.
    ResolvePermission {
        id: PermissionId,
        decision: PermissionDecision,
    },
    /// Take a point-in-time snapshot of the session's state.
    Snapshot,
    /// Start streaming this session's event feed on this connection.
    Subscribe,
    /// Tear down the session.
    CloseSession,
}

/// A response to an [`RpcRequest`], or an unsolicited pushed event.
///
/// `id` is `Some` for a direct reply to a request and `None` for an event
/// pushed asynchronously after a [`RpcRequestBody::Subscribe`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: Option<RequestCorrelationId>,
    pub body: RpcResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcResponseBody {
    SessionCreated { session_id: SessionId },
    Ack,
    Snapshot(SessionSnapshotWire),
    Event(AgentEventEnvelope),
    Error { message: String },
}

/// Serializable projection of `harness_runtime::session_client::SessionSnapshot`.
///
/// Defined here rather than reused directly because `harness-runtime`'s
/// `SessionSnapshot`/`SessionStatus` don't (and shouldn't need to) derive
/// `Serialize`/`Deserialize` for their in-process use, and `harness-protocol`
/// cannot depend on `harness-runtime` (the dependency runs the other way).
/// `harnessd` maps the runtime type into this one at the RPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshotWire {
    pub session_id: SessionId,
    pub status: SessionStatusWire,
    pub root_agent_id: AgentId,
    pub root_agent_status: AgentUsageSnapshot,
    pub usage: SessionUsageSnapshot,
    pub timestamp: Timestamp,
}

/// Wire counterpart of `harness_runtime::session_runtime::SessionStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatusWire {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_request_round_trips_through_json() {
        let request = RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::CreateSession {
                workspace_root: PathBuf::from("/tmp/workspace"),
                integration: "anthropic".to_string(),
                integration_config: serde_json::json!({ "api_key": "test" }),
                toolset: AgentToolset {
                    tools: std::collections::HashMap::new(),
                },
            },
        };
        let json = serde_json::to_string(&request).expect("serializable");
        let parsed: RpcRequest = serde_json::from_str(&json).expect("deserializable");
        assert_eq!(parsed.id, request.id);
        assert!(matches!(parsed.body, RpcRequestBody::CreateSession { .. }));
    }

    #[test]
    fn rpc_response_error_round_trips() {
        let response = RpcResponse {
            id: Some(RequestCorrelationId(42)),
            body: RpcResponseBody::Error {
                message: "boom".to_string(),
            },
        };
        let json = serde_json::to_string(&response).expect("serializable");
        let parsed: RpcResponse = serde_json::from_str(&json).expect("deserializable");
        assert_eq!(parsed.id, Some(RequestCorrelationId(42)));
        assert!(matches!(parsed.body, RpcResponseBody::Error { message } if message == "boom"));
    }
}

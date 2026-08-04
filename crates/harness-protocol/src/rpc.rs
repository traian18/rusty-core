
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

/// The current wire protocol version.
///
/// A client sends this in [`RpcRequestBody::Hello`] as the version it
/// speaks; the daemon compares it against its own [`PROTOCOL_VERSION`] and
/// responds with [`RpcResponseBody::Hello`] on a match or
/// [`RpcResponseBody::Error`] on a mismatch, so a client/daemon version skew
/// fails fast and clearly at connection time instead of manifesting as a
/// confusing mid-session deserialization error.
///
/// Bump this whenever a wire-incompatible change is made to [`RpcRequestBody`]
/// or [`RpcResponseBody`] (removing/renaming a variant or field, changing a
/// field's type/meaning). Purely additive changes (a new enum variant a
/// well-behaved client should tolerate) do not require a bump, though today's
/// `serde`-derived enums still fail closed on an unrecognized variant — see
/// the module-level docs for the caveat this leaves open.
pub const PROTOCOL_VERSION: u32 = 1;

/// Client-assigned correlation id, echoed back on the matching [`RpcResponse`]
/// so a caller can match responses to requests on a connection carrying
/// multiple in-flight requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestCorrelationId(pub u64);

/// A request sent to a running harness daemon over any transport.
///
/// Every request carries an explicit `session_id` (absent only for
/// [`RpcRequestBody::Hello`] and [`RpcRequestBody::CreateSession`]) so a
/// single connection can address any number of sessions rather than being
/// pinned to one session for its lifetime.
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
    /// Negotiate the wire protocol version before any other request.
    ///
    /// Every transport (`harness-transport-ipc`, `-websocket`, `-stdio`)
    /// rejects non-`Hello` requests on a connection that hasn't completed
    /// this handshake yet, so a version-mismatched client fails fast with a
    /// clear error instead of silently misbehaving later.
    Hello { protocol_version: u32 },
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
    ///
    /// When `since_seq` is `Some(n)`, every durable event with
    /// `session_sequence > n` is replayed (oldest first) before the live
    /// stream attaches, letting a reconnecting client resume without gaps or
    /// duplicates. `None` behaves exactly like the pre-resume protocol:
    /// live events only, starting from whatever arrives after the
    /// subscription is acknowledged.
    ///
    /// Only *durable* events are ever replayed this way — ephemeral events
    /// (`AssistantTextDelta`, `ReasoningDelta`, progress ticks; see
    /// `harness_session_store::is_durable`) are never persisted and so are
    /// unrecoverable after a disconnect. A reconnecting client sees the
    /// final assembled message/result, not the replayed keystrokes.
    Subscribe { since_seq: Option<u64> },
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
    /// Reply to [`RpcRequestBody::Hello`] on a successful version match.
    Hello {
        protocol_version: u32,
        capabilities: ProtocolCapabilities,
    },
    SessionCreated {
        session_id: SessionId,
    },
    Ack,
    Snapshot(SessionSnapshotWire),
    Event(AgentEventEnvelope),
    Error {
        message: String,
    },
}

/// Capabilities the daemon advertises during the [`RpcRequestBody::Hello`]
/// handshake.
///
/// Deliberately separate from [`crate::backend::BackendCapabilities`], which
/// describes a *model provider's* features (streaming, tool calls, ...);
/// this struct describes the *daemon/protocol's* own feature set so a client
/// can adapt its behavior (e.g. only attempt a resumed `Subscribe` if
/// `resumable_subscribe` is advertised) without a version bump for every new
/// optional capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    /// Whether `Subscribe { since_seq: Some(_) }` is supported.
    pub resumable_subscribe: bool,
}

impl Default for ProtocolCapabilities {
    fn default() -> Self {
        Self {
            resumable_subscribe: true,
        }
    }
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

    #[test]
    fn hello_request_round_trips_through_json() {
        let request = RpcRequest {
            id: RequestCorrelationId(0),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        };
        let json = serde_json::to_string(&request).expect("serializable");
        let parsed: RpcRequest = serde_json::from_str(&json).expect("deserializable");
        assert!(matches!(
            parsed.body,
            RpcRequestBody::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION
        ));
    }

    #[test]
    fn hello_response_round_trips_through_json() {
        let response = RpcResponse {
            id: Some(RequestCorrelationId(0)),
            body: RpcResponseBody::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities: ProtocolCapabilities::default(),
            },
        };
        let json = serde_json::to_string(&response).expect("serializable");
        let parsed: RpcResponse = serde_json::from_str(&json).expect("deserializable");
        match parsed.body {
            RpcResponseBody::Hello {
                protocol_version,
                capabilities,
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert!(capabilities.resumable_subscribe);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_since_seq_round_trips_through_json() {
        let request = RpcRequest {
            id: RequestCorrelationId(3),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Subscribe {
                since_seq: Some(42),
            },
        };
        let json = serde_json::to_string(&request).expect("serializable");
        let parsed: RpcRequest = serde_json::from_str(&json).expect("deserializable");
        assert!(matches!(
            parsed.body,
            RpcRequestBody::Subscribe {
                since_seq: Some(42)
            }
        ));
    }

    #[test]
    fn subscribe_without_since_seq_round_trips_through_json() {
        let request = RpcRequest {
            id: RequestCorrelationId(4),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Subscribe { since_seq: None },
        };
        let json = serde_json::to_string(&request).expect("serializable");
        let parsed: RpcRequest = serde_json::from_str(&json).expect("deserializable");
        assert!(matches!(
            parsed.body,
            RpcRequestBody::Subscribe { since_seq: None }
        ));
    }

    #[test]
    fn protocol_capabilities_default_advertises_resumable_subscribe() {
        let capabilities = ProtocolCapabilities::default();
        assert!(capabilities.resumable_subscribe);
    }
}

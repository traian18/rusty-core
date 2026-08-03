//! Behavioral counterpart of [`harness_protocol::rpc`]'s wire types.
//!
//! [`RpcHandler`] decouples "how bytes move" (the transport crates:
//! `harness-transport-ipc`, `-websocket`, `-stdio`, which depend only on this
//! trait and the wire types) from "what a request means" (implemented by
//! `apps/harnessd`, which wraps `Harness`/`SessionManager` and is the only
//! thing that actually knows how to construct a session). Transport crates
//! never depend on `harness-engine` directly; they only ever call through
//! this trait.

use async_trait::async_trait;
use tokio::sync::broadcast;

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{RpcRequestBody, RpcResponseBody};

/// Dispatches [`RpcRequestBody`] values against live sessions and exposes
/// their event streams for [`RpcRequestBody::Subscribe`].
///
/// # Object safety
///
/// `dyn`-compatible — every transport crate holds an `Arc<dyn RpcHandler>`.
#[async_trait]
pub trait RpcHandler: Send + Sync {
    /// Handle one request. `session_id` is `None` only for
    /// [`RpcRequestBody::CreateSession`]; implementations should treat a
    /// missing `session_id` on any other variant as an error rather than
    /// panicking, since it originates from untrusted wire input.
    async fn handle(&self, session_id: Option<SessionId>, body: RpcRequestBody) -> RpcResponseBody;

    /// Returns a fresh subscriber for `session_id`'s ordered event stream, or
    /// `None` if the session is unknown to this handler.
    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>>;
}

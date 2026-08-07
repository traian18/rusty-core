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

    /// Returns every durable event for `session_id` with `session_sequence`
    /// strictly greater than `since_seq`, ordered oldest first.
    ///
    /// Backs `Subscribe { since_seq: Some(_) }`'s resume path: a transport
    /// calls this before attaching the live receiver returned by
    /// [`subscribe`](Self::subscribe) so a reconnecting client can replay
    /// what it missed. The default implementation returns an empty backlog
    /// (equivalent to "no history available"), so existing/test
    /// implementations that don't back onto a durable session store keep
    /// compiling and behave like resume was never requested.
    ///
    /// Only durable events are ever returned here — see the durability note
    /// on `RpcRequestBody::Subscribe`.
    async fn events_since(
        &self,
        _session_id: SessionId,
        _since_seq: u64,
    ) -> Vec<AgentEventEnvelope> {
        Vec::new()
    }
}

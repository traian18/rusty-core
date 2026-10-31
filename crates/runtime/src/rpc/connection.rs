//! One client connection to the daemon, independent of how bytes travel.
//!
//! A transport reads frames, decodes them into [`RpcRequest`]s, and hands each to
//! [`RpcConnection::accept`]; every answer goes back through the transport's [`ResponseSink`].
//! The rules of the conversation live here once instead of in each transport: a connection must
//! start with `Hello`, `Hello` agrees on a protocol version, `Subscribe` replays missed events
//! and then streams live ones, and everything else is for the [`RpcHandler`].

use std::sync::Arc;

use async_trait::async_trait;
use rusty_protocol::{
    ids::SessionId,
    rpc::{
        ProtocolCapabilities, RequestCorrelationId, RpcError, RpcRequest, RpcRequestBody,
        RpcResponse, RpcResponseBody, PROTOCOL_VERSION,
    },
};
use tokio_util::sync::CancellationToken;

use super::{subscription::stream_events, RpcHandler};

/// Where a connection's responses go. Each transport frames and writes them its own way.
#[async_trait]
pub trait ResponseSink: Clone + Send + Sync + 'static {
    /// Sends one response. Returns `false` when the connection is going away.
    async fn send(&self, response: RpcResponse) -> bool;
}

/// The state of one connection: whether the handshake is done, and where to answer.
pub struct RpcConnection<S: ResponseSink> {
    handler: Arc<dyn RpcHandler>,
    sink: S,
    cancel: CancellationToken,
    hello_received: bool,
}

impl<S: ResponseSink> RpcConnection<S> {
    /// `cancel` ends the connection's background event streams when it fires.
    pub fn new(handler: Arc<dyn RpcHandler>, sink: S, cancel: CancellationToken) -> Self {
        Self {
            handler,
            sink,
            cancel,
            hello_received: false,
        }
    }

    /// Answers a frame that could not be decoded into a request.
    pub async fn reject_undecodable(&self, error: impl std::fmt::Display) {
        self.fail(
            None,
            RpcError::protocol(
                "protocol.invalid_request",
                format!("invalid request: {error}"),
            ),
        )
        .await;
    }

    /// Answers one decoded request. Anything before a successful `Hello` is refused.
    pub async fn accept(&mut self, request: RpcRequest) {
        let is_hello = matches!(request.body, RpcRequestBody::Hello { .. });
        if !self.hello_received && !is_hello {
            let error = RpcError::protocol(
                "protocol.hello_required",
                "Hello must be the first request on a connection",
            );
            self.fail(Some(request.id), error).await;
            return;
        }
        if let RpcRequestBody::Hello { protocol_version } = &request.body {
            self.hello_received = self.hello_received || *protocol_version == PROTOCOL_VERSION;
        }
        self.answer(request).await;
    }

    async fn answer(&self, request: RpcRequest) {
        let RpcRequest {
            id,
            session_id,
            body,
        } = request;
        match body {
            RpcRequestBody::Hello { protocol_version } => {
                self.answer_hello(id, protocol_version).await;
            }
            RpcRequestBody::Subscribe { since_seq } => {
                self.answer_subscribe(id, session_id, since_seq).await;
            }
            body => {
                let body = self.handler.handle(session_id, body).await;
                self.respond(Some(id), body).await;
            }
        }
    }

    async fn answer_hello(&self, id: RequestCorrelationId, client_version: u32) {
        let body = if client_version == PROTOCOL_VERSION {
            RpcResponseBody::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities: ProtocolCapabilities::default(),
            }
        } else {
            RpcResponseBody::Failure(RpcError::protocol(
                "protocol.version_mismatch",
                format!(
                    "protocol version mismatch: daemon speaks {PROTOCOL_VERSION}, client sent {client_version}"
                ),
            ))
        };
        self.respond(Some(id), body).await;
    }

    async fn answer_subscribe(
        &self,
        id: RequestCorrelationId,
        session_id: Option<SessionId>,
        since_seq: Option<u64>,
    ) {
        let Some(session_id) = session_id else {
            let error = RpcError::protocol(
                "request.missing_session_id",
                "Subscribe requires a session_id",
            );
            self.fail(Some(id), error).await;
            return;
        };
        let Some(receiver) = self.handler.subscribe(session_id) else {
            let error = RpcError::not_found("SESSION_NOT_FOUND", "unknown session");
            self.fail(Some(id), error).await;
            return;
        };
        self.respond(Some(id), RpcResponseBody::Ack).await;
        stream_events(
            self.handler.clone(),
            self.sink.clone(),
            self.cancel.clone(),
            session_id,
            since_seq,
            receiver,
        )
        .await;
    }

    async fn respond(&self, id: Option<RequestCorrelationId>, body: RpcResponseBody) {
        self.sink.send(RpcResponse { id, body }).await;
    }

    async fn fail(&self, id: Option<RequestCorrelationId>, error: RpcError) {
        self.respond(id, RpcResponseBody::Failure(error)).await;
    }
}

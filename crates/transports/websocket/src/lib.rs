#![warn(clippy::all)]

//! WebSocket transport: exposes an [`RpcHandler`] over a TCP + WebSocket
//! connection. Shares the exact `RpcRequest`/`RpcResponse`/`RpcHandler`
//! contract that `harness-transport-ipc` uses (see that crate's `PLAN.md`) —
//! this crate only differs in framing: each WebSocket text message carries
//! one JSON-encoded `RpcRequest`/`RpcResponse`, since WebSocket already
//! frames messages and no extra length-prefixing is needed.
//!
//! # Security
//!
//! Unlike a Unix socket (gated by filesystem permissions), a bound TCP
//! listener is reachable by anything that can route to it. This crate binds
//! whatever address it's given with no authentication of its own — if this
//! is ever bound to something other than loopback, add authentication (e.g.
//! a bearer token checked during the WebSocket handshake) at the call site
//! before doing so.

use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use harness_protocol::rpc::{RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody};
use harness_runtime::rpc::RpcHandler;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Binds `addr` and serves WebSocket connections until `shutdown` fires.
pub async fn serve(
    addr: SocketAddr,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_listener(listener, handler, shutdown).await
}

/// Serves WebSocket connections on an already-bound listener until
/// `shutdown` fires. Split out from [`serve`] so tests (and callers that
/// want the OS to pick a free port via `127.0.0.1:0`) can read the listener's
/// actual bound address before entering the accept loop.
pub async fn serve_listener(
    listener: TcpListener,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    tracing::info!(addr = %listener.local_addr()?, "harness-transport-websocket listening");
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _peer) = accepted?;
                let handler = handler.clone();
                let conn_shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, handler, conn_shutdown).await {
                        tracing::warn!(%error, "websocket connection ended with an error");
                    }
                });
            }
        }
    }
    Ok(())
}

async fn handle_connection(
    stream: TcpStream,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> Result<(), BoxError> {
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut write, mut read) = ws_stream.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let conn_cancel = CancellationToken::new();

    let writer_cancel = conn_cancel.clone();
    let writer_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_cancel.cancelled() => break,
                frame = out_rx.recv() => {
                    match frame {
                        Some(text) => {
                            if write.send(Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        let _ = write.close().await;
    });

    let result = read_loop(&mut read, &handler, &out_tx, &conn_cancel, &shutdown).await;

    conn_cancel.cancel();
    let _ = writer_task.await;
    result
}

async fn read_loop<S>(
    read: &mut S,
    handler: &Arc<dyn RpcHandler>,
    out_tx: &mpsc::Sender<String>,
    conn_cancel: &CancellationToken,
    shutdown: &CancellationToken,
) -> Result<(), BoxError>
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            message = read.next() => {
                let Some(message) = message else { return Ok(()) };
                match message? {
                    Message::Text(text) => {
                        let request: RpcRequest = match serde_json::from_str(&text) {
                            Ok(request) => request,
                            Err(error) => {
                                send(out_tx, RpcResponse {
                                    id: None,
                                    body: RpcResponseBody::Error {
                                        message: format!("invalid request: {error}"),
                                    },
                                }).await;
                                continue;
                            }
                        };
                        dispatch(request, handler, out_tx, conn_cancel).await;
                    }
                    Message::Close(_) => return Ok(()),
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                }
            }
        }
    }
}

async fn dispatch(
    request: RpcRequest,
    handler: &Arc<dyn RpcHandler>,
    out_tx: &mpsc::Sender<String>,
    conn_cancel: &CancellationToken,
) {
    let RpcRequest { id, session_id, body } = request;

    if matches!(body, RpcRequestBody::Subscribe) {
        let Some(session_id) = session_id else {
            send(out_tx, RpcResponse {
                id: Some(id),
                body: RpcResponseBody::Error {
                    message: "Subscribe requires a session_id".to_string(),
                },
            }).await;
            return;
        };
        match handler.subscribe(session_id) {
            Some(mut receiver) => {
                send(out_tx, RpcResponse { id: Some(id), body: RpcResponseBody::Ack }).await;
                let event_tx = out_tx.clone();
                let event_cancel = conn_cancel.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = event_cancel.cancelled() => break,
                            received = receiver.recv() => {
                                match received {
                                    Ok(envelope) => {
                                        let response = RpcResponse {
                                            id: None,
                                            body: RpcResponseBody::Event(envelope),
                                        };
                                        if !send(&event_tx, response).await {
                                            break;
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                        tracing::warn!(count, "websocket subscriber lagged; some events were dropped");
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                    }
                });
            }
            None => {
                send(out_tx, RpcResponse {
                    id: Some(id),
                    body: RpcResponseBody::Error { message: "unknown session".to_string() },
                }).await;
            }
        }
        return;
    }

    let response_body = handler.handle(session_id, body).await;
    send(out_tx, RpcResponse { id: Some(id), body: response_body }).await;
}

async fn send(out_tx: &mpsc::Sender<String>, response: RpcResponse) -> bool {
    let text = serde_json::to_string(&response).expect("RpcResponse always serializes");
    out_tx.send(text).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
    use harness_protocol::rpc::RequestCorrelationId;
    use tokio::sync::broadcast;
    use tokio_tungstenite::tungstenite::Message;

    struct FakeRpcHandler {
        events: broadcast::Sender<AgentEventEnvelope>,
        known_session: SessionId,
        calls: Mutex<Vec<RpcRequestBody>>,
    }

    impl FakeRpcHandler {
        fn new(known_session: SessionId) -> Self {
            let (events, _) = broadcast::channel(16);
            Self {
                events,
                known_session,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RpcHandler for FakeRpcHandler {
        async fn handle(
            &self,
            _session_id: Option<SessionId>,
            body: RpcRequestBody,
        ) -> RpcResponseBody {
            self.calls.lock().unwrap().push(body.clone());
            match body {
                RpcRequestBody::CreateSession { .. } => RpcResponseBody::SessionCreated {
                    session_id: self.known_session,
                },
                _ => RpcResponseBody::Ack,
            }
        }

        fn subscribe(
            &self,
            session_id: SessionId,
        ) -> Option<broadcast::Receiver<AgentEventEnvelope>> {
            if session_id == self.known_session {
                Some(self.events.subscribe())
            } else {
                None
            }
        }
    }

    async fn start_server(
        handler: Arc<FakeRpcHandler>,
    ) -> (SocketAddr, CancellationToken) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let shutdown = CancellationToken::new();
        let serve_handler: Arc<dyn RpcHandler> = handler;
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_listener(listener, serve_handler, serve_shutdown).await;
        });
        (addr, shutdown)
    }

    #[tokio::test]
    async fn request_response_round_trips() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (addr, _shutdown) = start_server(handler).await;

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        let request = RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::CreateSession {
                workspace_root: std::path::PathBuf::from("/tmp/ws"),
                integration: "anthropic".to_string(),
                integration_config: serde_json::json!({}),
                toolset: harness_protocol::tools::AgentToolset {
                    tools: HashMap::new(),
                },
            },
        };
        ws.send(Message::Text(serde_json::to_string(&request).unwrap()))
            .await
            .expect("send");

        let message = ws.next().await.expect("some message").expect("ok");
        let text = message.into_text().expect("text message");
        let response: RpcResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(response.id, Some(RequestCorrelationId(1)));
        assert!(matches!(
            response.body,
            RpcResponseBody::SessionCreated { session_id: sid } if sid == session_id
        ));
    }

    #[tokio::test]
    async fn subscribe_streams_events_after_ack() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let events_tx = handler.events.clone();
        let (addr, _shutdown) = start_server(handler).await;

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        let subscribe = RpcRequest {
            id: RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe,
        };
        ws.send(Message::Text(serde_json::to_string(&subscribe).unwrap()))
            .await
            .expect("send subscribe");

        let ack_message = ws.next().await.expect("ack").expect("ok");
        let ack: RpcResponse = serde_json::from_str(&ack_message.into_text().unwrap()).unwrap();
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        let envelope = AgentEventEnvelope {
            event_id: EventId::new(),
            session_id,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: 0,
            session_sequence: None,
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::RunStarted { run_id: RunId::new() },
        };
        events_tx.send(envelope).expect("send event");

        let pushed_message = ws.next().await.expect("pushed event").expect("ok");
        let pushed: RpcResponse = serde_json::from_str(&pushed_message.into_text().unwrap()).unwrap();
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }
}

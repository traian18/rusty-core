
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

use harness_protocol::rpc::ProtocolCapabilities;
use harness_protocol::rpc::{
    RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody, PROTOCOL_VERSION,
};
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
    let mut hello_received = false;
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
                                    body: RpcResponseBody::Failure(harness_protocol::rpc::RpcError::protocol(
                                        "protocol.invalid_request",
                                        format!("invalid request: {error}"),
                                    )),
                                }).await;
                                continue;
                            }
                        };
                        if !hello_received && !matches!(request.body, RpcRequestBody::Hello { .. }) {
                            send(out_tx, RpcResponse {
                                id: Some(request.id),
                                body: RpcResponseBody::Failure(harness_protocol::rpc::RpcError::protocol(
                                    "protocol.hello_required",
                                    "Hello must be the first request on a connection",
                                )),
                            }).await;
                            continue;
                        }
                        if !hello_received {
                            if let RpcRequestBody::Hello { protocol_version } = &request.body {
                                hello_received = *protocol_version == PROTOCOL_VERSION;
                            }
                        }
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
    let RpcRequest {
        id,
        session_id,
        body,
    } = request;

    if let RpcRequestBody::Hello { protocol_version } = body {
        let response_body = if protocol_version == PROTOCOL_VERSION {
            RpcResponseBody::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities: ProtocolCapabilities::default(),
            }
        } else {
            RpcResponseBody::Failure(harness_protocol::rpc::RpcError::protocol(
                "protocol.version_mismatch",
                format!(
                    "protocol version mismatch: daemon speaks {PROTOCOL_VERSION}, client sent {protocol_version}"
                ),
            ))
        };
        send(
            out_tx,
            RpcResponse {
                id: Some(id),
                body: response_body,
            },
        )
        .await;
        return;
    }

    if let RpcRequestBody::Subscribe { since_seq } = body {
        let Some(session_id) = session_id else {
            send(
                out_tx,
                RpcResponse {
                    id: Some(id),
                    body: RpcResponseBody::Failure(harness_protocol::rpc::RpcError::protocol(
                        "request.missing_session_id",
                        "Subscribe requires a session_id",
                    )),
                },
            )
            .await;
            return;
        };
        match handler.subscribe(session_id) {
            Some(mut receiver) => {
                send(
                    out_tx,
                    RpcResponse {
                        id: Some(id),
                        body: RpcResponseBody::Ack,
                    },
                )
                .await;

                let backlog = match since_seq {
                    Some(since_seq) => handler.events_since(session_id, since_seq).await,
                    None => Vec::new(),
                };
                let mut last_sent_seq = since_seq.unwrap_or(0);
                for envelope in backlog {
                    if let Some(seq) = envelope.session_sequence {
                        last_sent_seq = last_sent_seq.max(seq);
                    }
                    if !send(
                        out_tx,
                        RpcResponse {
                            id: None,
                            body: RpcResponseBody::Event(envelope),
                        },
                    )
                    .await
                    {
                        return;
                    }
                }

                let event_tx = out_tx.clone();
                let event_cancel = conn_cancel.clone();
                tokio::spawn(async move {
                    let mut last_sent_seq = last_sent_seq;
                    loop {
                        tokio::select! {
                            _ = event_cancel.cancelled() => break,
                            received = receiver.recv() => {
                                match received {
                                    Ok(envelope) => {
                                        if let Some(seq) = envelope.session_sequence {
                                            if seq <= last_sent_seq {
                                                continue;
                                            }
                                            last_sent_seq = seq;
                                        }
                                        let response = RpcResponse {
                                            id: None,
                                            body: RpcResponseBody::Event(envelope),
                                        };
                                        if !send(&event_tx, response).await {
                                            break;
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                        tracing::warn!(count, "websocket subscriber lagged; signalling an event gap");
                                        let gap = RpcResponse {
                                            id: None,
                                            body: RpcResponseBody::EventGap {
                                                session_id,
                                                last_delivered_sequence: last_sent_seq,
                                                dropped: count,
                                                cursor_expired: false,
                                            },
                                        };
                                        if !send(&event_tx, gap).await {
                                            break;
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                    }
                });
            }
            None => {
                send(
                    out_tx,
                    RpcResponse {
                        id: Some(id),
                        body: RpcResponseBody::Failure(harness_protocol::rpc::RpcError::not_found(
                            "SESSION_NOT_FOUND",
                            "unknown session",
                        )),
                    },
                )
                .await;
            }
        }
        return;
    }

    let response_body = handler.handle(session_id, body).await;
    send(
        out_tx,
        RpcResponse {
            id: Some(id),
            body: response_body,
        },
    )
    .await;
}

async fn send(out_tx: &mpsc::Sender<String>, response: RpcResponse) -> bool {
    let text = serde_json::to_string(&response).expect("RpcResponse always serializes");
    out_tx.send(text).await.is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;

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
        backlog: Mutex<Vec<AgentEventEnvelope>>,
    }

    impl FakeRpcHandler {
        fn new(known_session: SessionId) -> Self {
            let (events, _) = broadcast::channel(16);
            Self {
                events,
                known_session,
                calls: Mutex::new(Vec::new()),
                backlog: Mutex::new(Vec::new()),
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

        async fn events_since(
            &self,
            session_id: SessionId,
            since_seq: u64,
        ) -> Vec<AgentEventEnvelope> {
            if session_id != self.known_session {
                return Vec::new();
            }
            self.backlog
                .lock()
                .unwrap()
                .iter()
                .filter(|envelope| envelope.session_sequence.is_some_and(|seq| seq > since_seq))
                .cloned()
                .collect()
        }
    }

    #[allow(dead_code)]
    fn make_envelope(session_id: SessionId, session_sequence: u64) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: session_sequence,
            session_sequence: Some(session_sequence),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::RunStarted {
                run_id: RunId::new(),
            },
        }
    }

    async fn start_server(handler: Arc<FakeRpcHandler>) -> (SocketAddr, CancellationToken) {
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

    async fn recv_response(
        ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    ) -> RpcResponse {
        let message = ws.next().await.expect("some message").expect("ok");
        let text = message.into_text().expect("text message");
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn hello_negotiates_matching_protocol_version() {
        let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
        let (addr, _shutdown) = start_server(handler).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            })
            .unwrap(),
        ))
        .await
        .expect("send hello");

        let response = recv_response(&mut ws).await;
        match response.body {
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

    #[tokio::test]
    async fn hello_rejects_a_mismatched_protocol_version() {
        let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
        let (addr, _shutdown) = start_server(handler).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION + 1,
                },
            })
            .unwrap(),
        ))
        .await
        .expect("send hello");

        let response = recv_response(&mut ws).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(2),
                session_id: Some(SessionId::new()),
                body: RpcRequestBody::Snapshot,
            })
            .unwrap(),
        ))
        .await
        .expect("send request after rejected hello");
        let response = recv_response(&mut ws).await;
        assert!(matches!(
            response.body,
            RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
        ));
    }

    #[tokio::test]
    async fn requests_before_hello_are_rejected() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (addr, _shutdown) = start_server(handler).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: Some(session_id),
                body: RpcRequestBody::Snapshot,
            })
            .unwrap(),
        ))
        .await
        .expect("send");

        let response = recv_response(&mut ws).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));
    }

    #[tokio::test]
    async fn request_response_round_trips() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (addr, _shutdown) = start_server(handler).await;

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            })
            .unwrap(),
        ))
        .await
        .expect("send hello");
        let hello_response = recv_response(&mut ws).await;
        assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

        let request = RpcRequest {
            id: RequestCorrelationId(2),
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

        let response = recv_response(&mut ws).await;
        assert_eq!(response.id, Some(RequestCorrelationId(2)));
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

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            })
            .unwrap(),
        ))
        .await
        .expect("send hello");
        let hello_response = recv_response(&mut ws).await;
        assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

        let subscribe = RpcRequest {
            id: RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: None },
        };
        ws.send(Message::Text(serde_json::to_string(&subscribe).unwrap()))
            .await
            .expect("send subscribe");

        let ack = recv_response(&mut ws).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        events_tx
            .send(make_envelope(session_id, 1))
            .expect("send event");

        let pushed = recv_response(&mut ws).await;
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }

    #[tokio::test]
    async fn subscribe_with_since_seq_replays_backlog_and_dedupes_live_events() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        {
            let mut backlog = handler.backlog.lock().unwrap();
            backlog.push(make_envelope(session_id, 1));
            backlog.push(make_envelope(session_id, 2));
        }
        let events_tx = handler.events.clone();
        let (addr, _shutdown) = start_server(handler).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            })
            .unwrap(),
        ))
        .await
        .expect("send hello");
        let hello_response = recv_response(&mut ws).await;
        assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

        ws.send(Message::Text(
            serde_json::to_string(&RpcRequest {
                id: RequestCorrelationId(3),
                session_id: Some(session_id),
                body: RpcRequestBody::Subscribe { since_seq: Some(0) },
            })
            .unwrap(),
        ))
        .await
        .expect("send subscribe");

        let ack = recv_response(&mut ws).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        let first = recv_response(&mut ws).await;
        let second = recv_response(&mut ws).await;
        let seqs: Vec<u64> = [first, second]
            .into_iter()
            .map(|r| match r.body {
                RpcResponseBody::Event(envelope) => envelope.session_sequence.unwrap(),
                other => panic!("expected replayed Event, got {other:?}"),
            })
            .collect();
        assert_eq!(seqs, vec![1, 2]);

        events_tx
            .send(make_envelope(session_id, 2))
            .expect("send duplicate event");
        events_tx
            .send(make_envelope(session_id, 3))
            .expect("send fresh event");

        let third = recv_response(&mut ws).await;
        match third.body {
            RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
            other => panic!("expected Event with seq 3, got {other:?}"),
        }
    }
}

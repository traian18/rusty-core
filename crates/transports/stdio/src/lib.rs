#![warn(clippy::all)]

//! Stdio transport: exposes an [`RpcHandler`] over stdin/stdout using
//! newline-delimited JSON (ND-JSON). Shares the exact
//! `RpcRequest`/`RpcResponse`/`RpcHandler` contract that
//! `harness-transport-ipc` uses (see that crate's `PLAN.md`) — this crate
//! only differs in framing: one `serde_json`-encoded line per request or
//! response, deliberately simpler than LSP's `Content-Length:` header
//! framing since nothing here needs binary-safe payloads.
//!
//! # Stdout ownership
//!
//! Once [`serve`] is running, stdout is reserved **exclusively** for the RPC
//! stream — any other writer to stdout (a stray `println!`, a panic hook, a
//! dependency's default logger) corrupts the line-delimited framing. Callers
//! must route all logging to stderr before calling this.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_protocol::rpc::ProtocolCapabilities;
use harness_protocol::rpc::{
    RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody, PROTOCOL_VERSION,
};
use harness_runtime::rpc::RpcHandler;

/// Serves the process's real stdin/stdout until `shutdown` fires.
pub async fn serve(
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    serve_io(tokio::io::stdin(), tokio::io::stdout(), handler, shutdown).await
}

/// Serves an arbitrary reader/writer pair as the ND-JSON RPC stream.
///
/// Split out from [`serve`] so tests can drive this over an in-memory
/// `tokio::io::duplex()` pair instead of spawning a real subprocess.
pub async fn serve_io<R, W>(
    reader: R,
    writer: W,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let conn_cancel = CancellationToken::new();

    // The writer task owns the writer exclusively so response lines and
    // pushed event lines never interleave mid-line.
    let writer_cancel = conn_cancel.clone();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        loop {
            tokio::select! {
                _ = writer_cancel.cancelled() => break,
                line = out_rx.recv() => {
                    match line {
                        Some(text) => {
                            if writer.write_all(text.as_bytes()).await.is_err() {
                                break;
                            }
                            if writer.write_all(b"\n").await.is_err() {
                                break;
                            }
                            if writer.flush().await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    let mut lines = BufReader::new(reader).lines();
    let result = read_loop(&mut lines, &handler, &out_tx, &conn_cancel, &shutdown).await;

    conn_cancel.cancel();
    let _ = writer_task.await;
    result
}

async fn read_loop<R>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    handler: &Arc<dyn RpcHandler>,
    out_tx: &mpsc::Sender<String>,
    conn_cancel: &CancellationToken,
    shutdown: &CancellationToken,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut hello_received = false;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) }; // clean EOF
                if line.trim().is_empty() {
                    continue;
                }
                let request: RpcRequest = match serde_json::from_str(&line) {
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
                                        tracing::warn!(count, "stdio subscriber lagged; signalling an event gap");
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
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
    use harness_protocol::rpc::RequestCorrelationId;
    use tokio::io::AsyncReadExt;
    use tokio::sync::broadcast;

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

    async fn read_response_line<R: AsyncRead + Unpin>(reader: &mut R) -> RpcResponse {
        let mut buf = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte).await.expect("read byte");
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        serde_json::from_slice(&buf).expect("valid RpcResponse json")
    }

    async fn write_request<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, request: &RpcRequest) {
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        writer.write_all(&line).await.unwrap();
    }

    #[tokio::test]
    async fn hello_negotiates_matching_protocol_version() {
        let session_id = SessionId::new();
        let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

        let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;

        let response = read_response_line(&mut client_out_read).await;
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
        let session_id = SessionId::new();
        let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

        let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION + 1,
                },
            },
        )
        .await;

        let response = read_response_line(&mut client_out_read).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(2),
                session_id: Some(session_id),
                body: RpcRequestBody::Snapshot,
            },
        )
        .await;
        let response = read_response_line(&mut client_out_read).await;
        assert!(matches!(
            response.body,
            RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
        ));
    }

    #[tokio::test]
    async fn requests_before_hello_are_rejected() {
        let session_id = SessionId::new();
        let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

        let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: Some(session_id),
                body: RpcRequestBody::Snapshot,
            },
        )
        .await;

        let response = read_response_line(&mut client_out_read).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));
    }

    #[tokio::test]
    async fn request_response_round_trips() {
        let session_id = SessionId::new();
        let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

        let (client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        let mut client_in_write = client_in_write;
        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let hello_response = read_response_line(&mut client_out_read).await;
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
        write_request(&mut client_in_write, &request).await;

        let response = read_response_line(&mut client_out_read).await;
        assert_eq!(response.id, Some(RequestCorrelationId(2)));
        assert!(matches!(
            response.body,
            RpcResponseBody::SessionCreated { session_id: sid } if sid == session_id
        ));
    }

    #[tokio::test]
    async fn subscribe_streams_events_after_ack() {
        let session_id = SessionId::new();
        let handler_impl = Arc::new(FakeRpcHandler::new(session_id));
        let events_tx = handler_impl.events.clone();
        let handler: Arc<dyn RpcHandler> = handler_impl;

        let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let hello_response = read_response_line(&mut client_out_read).await;
        assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(7),
                session_id: Some(session_id),
                body: RpcRequestBody::Subscribe { since_seq: None },
            },
        )
        .await;

        let ack = read_response_line(&mut client_out_read).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        events_tx
            .send(make_envelope(session_id, 1))
            .expect("send event");

        let pushed = read_response_line(&mut client_out_read).await;
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }

    #[tokio::test]
    async fn subscribe_with_since_seq_replays_backlog_and_dedupes_live_events() {
        let session_id = SessionId::new();
        let handler_impl = Arc::new(FakeRpcHandler::new(session_id));
        {
            let mut backlog = handler_impl.backlog.lock().unwrap();
            backlog.push(make_envelope(session_id, 1));
            backlog.push(make_envelope(session_id, 2));
        }
        let events_tx = handler_impl.events.clone();
        let handler: Arc<dyn RpcHandler> = handler_impl;

        let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
        let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
        });

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let hello_response = read_response_line(&mut client_out_read).await;
        assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

        write_request(
            &mut client_in_write,
            &RpcRequest {
                id: RequestCorrelationId(3),
                session_id: Some(session_id),
                body: RpcRequestBody::Subscribe { since_seq: Some(0) },
            },
        )
        .await;

        let ack = read_response_line(&mut client_out_read).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        let first = read_response_line(&mut client_out_read).await;
        let second = read_response_line(&mut client_out_read).await;
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

        let third = read_response_line(&mut client_out_read).await;
        match third.body {
            RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
            other => panic!("expected Event with seq 3, got {other:?}"),
        }
    }
}

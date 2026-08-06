#![warn(clippy::all)]

//! Local IPC transport: exposes an [`RpcHandler`] over a Unix domain socket.
//!
//! This crate only knows how to move framed `RpcRequest`/`RpcResponse` bytes
//! across a `UnixStream` and dispatch them against an [`RpcHandler`] — it has
//! no knowledge of `Harness`, `SessionManager`, or any concrete session type.
//! `apps/harnessd` implements `RpcHandler` and is the only thing that knows
//! what a request actually means.

pub mod framing;

use std::path::Path;
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_protocol::rpc::ProtocolCapabilities;
use harness_protocol::rpc::{
    RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody, PROTOCOL_VERSION,
};
use harness_runtime::rpc::RpcHandler;

use framing::{read_frame, write_frame};

/// Binds `socket_path` and serves connections until `shutdown` fires.
///
/// Removes a stale socket file at `socket_path` first — Unix sockets don't
/// clean up their own path after a crash, and `UnixListener::bind` fails with
/// `AddrInUse` on a leftover path even though nothing is listening on it.
pub async fn serve(
    socket_path: &Path,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    tracing::info!(path = %socket_path.display(), "harness-transport-ipc listening");

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                let handler = handler.clone();
                let conn_shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, handler, conn_shutdown).await {
                        tracing::warn!(%error, "ipc connection ended with an error");
                    }
                });
            }
        }
    }
    Ok(())
}

/// Drives a single accepted connection: reads request frames, dispatches
/// each against `handler`, and writes response frames back. A
/// [`RpcRequestBody::Subscribe`] spins up a background task that forwards
/// the session's event stream onto the same connection for its lifetime.
async fn handle_connection(
    stream: UnixStream,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let (mut read_half, mut write_half) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let conn_cancel = CancellationToken::new();

    // The writer task owns the write half exclusively so response frames and
    // pushed event frames never interleave mid-frame.
    let writer_cancel = conn_cancel.clone();
    let writer_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_cancel.cancelled() => break,
                frame = out_rx.recv() => {
                    match frame {
                        Some(bytes) => {
                            if write_frame(&mut write_half, &bytes).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    let result = read_loop(
        &mut read_half,
        handler,
        out_tx,
        conn_cancel.clone(),
        shutdown,
    )
    .await;

    conn_cancel.cancel();
    let _ = writer_task.await;
    result
}

async fn read_loop(
    read_half: &mut tokio::net::unix::OwnedReadHalf,
    handler: Arc<dyn RpcHandler>,
    out_tx: mpsc::Sender<Vec<u8>>,
    conn_cancel: CancellationToken,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let mut hello_received = false;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            frame = read_frame(read_half) => {
                let bytes = match frame? {
                    Some(bytes) => bytes,
                    None => return Ok(()), // peer closed the connection
                };
                let request: RpcRequest = match serde_json::from_slice(&bytes) {
                    Ok(request) => request,
                    Err(error) => {
                        send(&out_tx, RpcResponse {
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
                    send(&out_tx, RpcResponse {
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
                dispatch(request, &handler, &out_tx, &conn_cancel).await;
            }
        }
    }
}

async fn dispatch(
    request: RpcRequest,
    handler: &Arc<dyn RpcHandler>,
    out_tx: &mpsc::Sender<Vec<u8>>,
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
                                        // The backlog (drained above) and the live
                                        // broadcast receiver (subscribed before the
                                        // backlog fetch) can overlap on events
                                        // durably appended in that gap; skip anything
                                        // already replayed so the resumed stream has
                                        // no duplicates.
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
                                        tracing::warn!(count, "ipc subscriber lagged; signalling an event gap");
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

/// Serializes and sends one response frame. Returns `false` if the writer
/// task has already exited (the connection is going away).
async fn send(out_tx: &mpsc::Sender<Vec<u8>>, response: RpcResponse) -> bool {
    let bytes = serde_json::to_vec(&response).expect("RpcResponse always serializes");
    out_tx.send(bytes).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
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

    async fn connect_and_serve(
        handler: Arc<FakeRpcHandler>,
    ) -> (UnixStream, tempfile::TempDir, CancellationToken) {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("harness.sock");
        let shutdown = CancellationToken::new();

        let serve_path = socket_path.clone();
        let serve_handler: Arc<dyn RpcHandler> = handler;
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve(&serve_path, serve_handler, serve_shutdown).await;
        });

        // Give the listener a moment to bind before connecting.
        for _ in 0..50 {
            if UnixStream::connect(&socket_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let stream = UnixStream::connect(&socket_path)
            .await
            .expect("connect to freshly bound socket");
        (stream, dir, shutdown)
    }

    async fn send_request(stream: &mut UnixStream, request: &RpcRequest) {
        write_frame(stream, &serde_json::to_vec(request).unwrap())
            .await
            .expect("write request");
    }

    async fn read_response(stream: &mut UnixStream) -> RpcResponse {
        let bytes = read_frame(stream)
            .await
            .expect("read")
            .expect("some response");
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn do_hello(stream: &mut UnixStream, id: u64) {
        send_request(
            stream,
            &RpcRequest {
                id: harness_protocol::rpc::RequestCorrelationId(id),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let response = read_response(stream).await;
        assert!(matches!(response.body, RpcResponseBody::Hello { .. }));
    }

    #[tokio::test]
    async fn hello_negotiates_matching_protocol_version() {
        let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

        send_request(
            &mut stream,
            &RpcRequest {
                id: harness_protocol::rpc::RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;

        let response = read_response(&mut stream).await;
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
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

        send_request(
            &mut stream,
            &RpcRequest {
                id: harness_protocol::rpc::RequestCorrelationId(1),
                session_id: None,
                body: RpcRequestBody::Hello {
                    protocol_version: PROTOCOL_VERSION + 1,
                },
            },
        )
        .await;

        let response = read_response(&mut stream).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));

        send_request(
            &mut stream,
            &RpcRequest {
                id: harness_protocol::rpc::RequestCorrelationId(2),
                session_id: Some(SessionId::new()),
                body: RpcRequestBody::Snapshot,
            },
        )
        .await;
        let response = read_response(&mut stream).await;
        assert!(matches!(
            response.body,
            RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
        ));
    }

    #[tokio::test]
    async fn requests_before_hello_are_rejected() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

        send_request(
            &mut stream,
            &RpcRequest {
                id: harness_protocol::rpc::RequestCorrelationId(1),
                session_id: Some(session_id),
                body: RpcRequestBody::Snapshot,
            },
        )
        .await;

        let response = read_response(&mut stream).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));

        // Hello still succeeds afterward on the same connection.
        do_hello(&mut stream, 2).await;
    }

    #[tokio::test]
    async fn request_response_round_trips() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
        do_hello(&mut stream, 1).await;

        let request = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(2),
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
        send_request(&mut stream, &request).await;

        let response = read_response(&mut stream).await;
        assert_eq!(
            response.id,
            Some(harness_protocol::rpc::RequestCorrelationId(2))
        );
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
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
        do_hello(&mut stream, 1).await;

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: None },
        };
        send_request(&mut stream, &subscribe).await;

        let ack = read_response(&mut stream).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        let envelope = make_envelope(session_id, 1);
        events_tx.send(envelope).expect("send event");

        let pushed = read_response(&mut stream).await;
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }

    #[tokio::test]
    async fn subscribe_to_unknown_session_errors() {
        let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
        do_hello(&mut stream, 1).await;

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(9),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Subscribe { since_seq: None },
        };
        send_request(&mut stream, &subscribe).await;

        let response = read_response(&mut stream).await;
        assert!(matches!(response.body, RpcResponseBody::Failure(_)));
    }

    #[tokio::test]
    async fn subscribe_with_since_seq_replays_backlog_before_live_events() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        {
            let mut backlog = handler.backlog.lock().unwrap();
            backlog.push(make_envelope(session_id, 1));
            backlog.push(make_envelope(session_id, 2));
        }
        let events_tx = handler.events.clone();
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
        do_hello(&mut stream, 1).await;

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(3),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: Some(0) },
        };
        send_request(&mut stream, &subscribe).await;

        let ack = read_response(&mut stream).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        let first = read_response(&mut stream).await;
        let second = read_response(&mut stream).await;
        let seqs: Vec<u64> = [first, second]
            .into_iter()
            .map(|r| match r.body {
                RpcResponseBody::Event(envelope) => envelope.session_sequence.unwrap(),
                other => panic!("expected replayed Event, got {other:?}"),
            })
            .collect();
        assert_eq!(seqs, vec![1, 2]);

        // A live event with a sequence already covered by the backlog is
        // deduplicated (never forwarded); one past the backlog is forwarded.
        events_tx
            .send(make_envelope(session_id, 2))
            .expect("send duplicate event");
        events_tx
            .send(make_envelope(session_id, 3))
            .expect("send fresh event");

        let third = read_response(&mut stream).await;
        match third.body {
            RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
            other => panic!("expected Event with seq 3, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_without_since_seq_skips_backlog() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        {
            let mut backlog = handler.backlog.lock().unwrap();
            backlog.push(make_envelope(session_id, 1));
        }
        let events_tx = handler.events.clone();
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
        do_hello(&mut stream, 1).await;

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(4),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: None },
        };
        send_request(&mut stream, &subscribe).await;

        let ack = read_response(&mut stream).await;
        assert!(matches!(ack.body, RpcResponseBody::Ack));

        events_tx
            .send(make_envelope(session_id, 5))
            .expect("send live event");
        let pushed = read_response(&mut stream).await;
        match pushed.body {
            RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(5)),
            other => panic!("expected Event with seq 5, got {other:?}"),
        }
    }
}

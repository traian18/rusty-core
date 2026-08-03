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

use harness_protocol::rpc::{RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody};
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

    let result = read_loop(&mut read_half, handler, out_tx, conn_cancel.clone(), shutdown).await;

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
                            body: RpcResponseBody::Error {
                                message: format!("invalid request: {error}"),
                            },
                        }).await;
                        continue;
                    }
                };
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
                                        tracing::warn!(count, "ipc subscriber lagged; some events were dropped");
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

    #[tokio::test]
    async fn request_response_round_trips() {
        let session_id = SessionId::new();
        let handler = Arc::new(FakeRpcHandler::new(session_id));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

        let request = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(1),
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
        write_frame(&mut stream, &serde_json::to_vec(&request).unwrap())
            .await
            .expect("write request");

        let response_bytes = read_frame(&mut stream)
            .await
            .expect("read")
            .expect("some response");
        let response: RpcResponse = serde_json::from_slice(&response_bytes).unwrap();
        assert_eq!(response.id, Some(harness_protocol::rpc::RequestCorrelationId(1)));
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

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe,
        };
        write_frame(&mut stream, &serde_json::to_vec(&subscribe).unwrap())
            .await
            .expect("write subscribe");

        let ack_bytes = read_frame(&mut stream).await.expect("read").expect("ack");
        let ack: RpcResponse = serde_json::from_slice(&ack_bytes).unwrap();
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

        let pushed_bytes = read_frame(&mut stream)
            .await
            .expect("read")
            .expect("pushed event frame");
        let pushed: RpcResponse = serde_json::from_slice(&pushed_bytes).unwrap();
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }

    #[tokio::test]
    async fn subscribe_to_unknown_session_errors() {
        let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
        let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

        let subscribe = RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(9),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Subscribe,
        };
        write_frame(&mut stream, &serde_json::to_vec(&subscribe).unwrap())
            .await
            .expect("write subscribe");

        let response_bytes = read_frame(&mut stream).await.expect("read").expect("response");
        let response: RpcResponse = serde_json::from_slice(&response_bytes).unwrap();
        assert!(matches!(response.body, RpcResponseBody::Error { .. }));
    }
}

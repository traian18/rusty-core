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

use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_protocol::rpc::{RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody};
use harness_runtime::rpc::RpcHandler;

/// Serves the process's real stdin/stdout until `shutdown` fires.
pub async fn serve(handler: Arc<dyn RpcHandler>, shutdown: CancellationToken) -> std::io::Result<()> {
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
                            body: RpcResponseBody::Error {
                                message: format!("invalid request: {error}"),
                            },
                        }).await;
                        continue;
                    }
                };
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
                                        tracing::warn!(count, "stdio subscriber lagged; some events were dropped");
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
    use tokio::io::AsyncReadExt;
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
        let mut line = serde_json::to_vec(&request).unwrap();
        line.push(b'\n');
        client_in_write.write_all(&line).await.unwrap();

        let response = read_response_line(&mut client_out_read).await;
        assert_eq!(response.id, Some(RequestCorrelationId(1)));
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

        let subscribe = RpcRequest {
            id: RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe,
        };
        let mut line = serde_json::to_vec(&subscribe).unwrap();
        line.push(b'\n');
        client_in_write.write_all(&line).await.unwrap();

        let ack = read_response_line(&mut client_out_read).await;
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

        let pushed = read_response_line(&mut client_out_read).await;
        assert!(pushed.id.is_none());
        assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
    }
}

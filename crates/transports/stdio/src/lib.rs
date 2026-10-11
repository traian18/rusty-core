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
                answer_one_request(request, handler, out_tx, conn_cancel).await;
            }
        }
    }
}

/// Answers one decoded request: completes the `Hello` handshake itself and hands
/// every other request to the [`RpcHandler`], writing its response (and any
/// streamed events) back to this connection.
async fn answer_one_request(
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
mod tests;

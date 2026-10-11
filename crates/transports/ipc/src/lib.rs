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
                answer_one_request(request, &handler, &out_tx, &conn_cancel).await;
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
mod tests;

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
                        answer_one_request(request, handler, out_tx, conn_cancel).await;
                    }
                    Message::Close(_) => return Ok(()),
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                }
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
mod tests;

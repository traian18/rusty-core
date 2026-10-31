#![warn(clippy::all)]

//! WebSocket transport: exposes an [`RpcHandler`] over a TCP + WebSocket
//! connection. Shares the exact `RpcRequest`/`RpcResponse`/`RpcHandler`
//! contract that `rusty-transport-ipc` uses (see that crate's `PLAN.md`) —
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

use async_trait::async_trait;

use rusty_protocol::rpc::{RpcRequest, RpcResponse};
use rusty_runtime::rpc::{ResponseSink, RpcConnection, RpcHandler};

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
    tracing::info!(addr = %listener.local_addr()?, "rusty-transport-websocket listening");
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
    let mut connection = RpcConnection::new(
        handler.clone(),
        FrameSink(out_tx.clone()),
        conn_cancel.clone(),
    );
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
                connection.reject_undecodable(error).await;
                continue;
            }
        };
        connection.accept(request).await;
                            }
                            Message::Close(_) => return Ok(()),
                            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                        }
                    }
                }
    }
}

/// Frames each response for this transport and queues it for the connection's writer task.
#[derive(Clone)]
struct FrameSink(mpsc::Sender<String>);

#[async_trait]
impl ResponseSink for FrameSink {
    async fn send(&self, response: RpcResponse) -> bool {
        match serde_json::to_string(&response) {
            Ok(frame) => self.0.send(frame).await.is_ok(),
            Err(error) => {
                tracing::error!(%error, "a response failed to serialize and was dropped");
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;

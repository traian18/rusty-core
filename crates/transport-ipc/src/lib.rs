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

use async_trait::async_trait;

use rusty_protocol::rpc::{RpcRequest, RpcResponse};
use rusty_runtime::rpc::{ResponseSink, RpcConnection, RpcHandler};

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
    tracing::info!(path = %socket_path.display(), "rusty-transport-ipc listening");

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
    let mut connection = RpcConnection::new(handler, FrameSink(out_tx), conn_cancel);
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
                connection.reject_undecodable(error).await;
                continue;
            }
        };
        connection.accept(request).await;
                    }
                }
    }
}

/// Frames each response for this transport and queues it for the connection's writer task.
#[derive(Clone)]
struct FrameSink(mpsc::Sender<Vec<u8>>);

#[async_trait]
impl ResponseSink for FrameSink {
    async fn send(&self, response: RpcResponse) -> bool {
        match serde_json::to_vec(&response) {
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

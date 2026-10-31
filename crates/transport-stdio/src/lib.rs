#![warn(clippy::all)]

//! Stdio transport: exposes an [`RpcHandler`] over stdin/stdout using
//! newline-delimited JSON (ND-JSON). Shares the exact
//! `RpcRequest`/`RpcResponse`/`RpcHandler` contract that
//! `rusty-transport-ipc` uses (see that crate's `PLAN.md`) — this crate
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

use async_trait::async_trait;

use rusty_protocol::rpc::{RpcRequest, RpcResponse};
use rusty_runtime::rpc::{ResponseSink, RpcConnection, RpcHandler};

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
    let mut connection = RpcConnection::new(
        handler.clone(),
        FrameSink(out_tx.clone()),
        conn_cancel.clone(),
    );
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

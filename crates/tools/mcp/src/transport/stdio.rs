//! Stdio transport: spawns a server process and speaks newline-delimited
//! JSON-RPC over its stdin/stdout.
//!
//! One line per message, no embedded newlines, per MCP's stdio transport —
//! the same framing discipline `harness-transport-stdio` uses for the
//! harness's own wire protocol, but this is a separate, from-scratch
//! implementation: the message shapes (JSON-RPC method names, params) are
//! MCP's, not the harness's, so there's nothing to share beyond the
//! "newline-delimited JSON over a pipe" idea.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::error::McpError;
use crate::protocol::{JsonRpcInbound, JsonRpcNotification, JsonRpcRequest};
use crate::transport::McpTransport;

type PendingMap = Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>;

/// A live stdio connection to one MCP server process.
pub(crate) struct StdioTransport {
    stdin: Mutex<ChildStdin>,
    pending: Arc<PendingMap>,
    next_id: AtomicU64,
    child: Mutex<Child>,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    request_timeout: Duration,
}

impl StdioTransport {
    /// Spawn the server process and start its reader/stderr pumps.
    ///
    /// Does **not** perform the `initialize` handshake — that's
    /// [`crate::client::McpClient`]'s job, and it is identical for every
    /// transport.
    pub(crate) async fn connect(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&str>,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut builder = Command::new(command);
        builder
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            builder.current_dir(cwd);
        }

        let mut child = builder.spawn().map_err(|source| McpError::Spawn {
            name: server_name.to_owned(),
            source,
        })?;
        let stdin = child.stdin.take().expect("stdin piped at spawn");
        let stdout = child.stdout.take().expect("stdout piped at spawn");
        let stderr = child.stderr.take().expect("stderr piped at spawn");

        let pending: Arc<PendingMap> = Arc::new(Mutex::new(HashMap::new()));
        let stderr_task = tokio::spawn(log_stderr(server_name.to_owned(), stderr));
        let reader_task = tokio::spawn(read_loop(server_name.to_owned(), stdout, pending.clone()));

        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            reader_task,
            stderr_task,
            request_timeout,
        })
    }

    async fn write_line(&self, line: &str) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;
        line.push('\n');

        if let Err(err) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(err);
        }

        match timeout(self.request_timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            // Sender dropped without a reply — the reader loop only does
            // this when the connection closed, and it already resolved
            // every pending request to `Err(Closed)` before dropping.
            Ok(Err(_)) => Err(McpError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Timeout(self.request_timeout))
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let mut line = serde_json::to_string(&JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        })?;
        line.push('\n');
        self.write_line(&line).await
    }

    async fn shutdown(&self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        let _ = self.child.lock().await.kill().await;
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        // Best-effort: `Command::kill_on_drop(true)` already reaps the
        // child once its handle drops, but that only fires once `child`
        // itself (inside the Mutex) is dropped along with `self`.
    }
}

/// Reads newline-delimited JSON-RPC responses from the server and resolves
/// the matching pending request. Requests/notifications the server sends
/// unprompted (sampling, roots, logging, progress) aren't supported by this
/// client yet and are ignored rather than erroring the connection.
async fn read_loop(server_name: String, stdout: ChildStdout, pending: Arc<PendingMap>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                handle_line(&server_name, &line, &pending).await;
            }
            Ok(None) => break, // clean EOF
            Err(err) => {
                warn!(server = %server_name, error = %err, "MCP: stdout read error");
                break;
            }
        }
    }

    // The connection is gone — fail everything still waiting rather than
    // let those callers hang until their timeout.
    for (_, tx) in pending.lock().await.drain() {
        let _ = tx.send(Err(McpError::Closed));
    }
}

async fn handle_line(server_name: &str, line: &str, pending: &Arc<PendingMap>) {
    let inbound: JsonRpcInbound = match serde_json::from_str(line) {
        Ok(inbound) => inbound,
        Err(err) => {
            warn!(server = %server_name, error = %err, "MCP: failed to parse server message");
            return;
        }
    };

    // A message with `method` set is a request/notification *from* the
    // server, not a reply to one of ours — out of scope for v1.
    if inbound.method.is_some() {
        return;
    }

    let Some(id) = inbound.id.as_ref().and_then(Value::as_u64) else {
        return;
    };

    let outcome = match inbound.error {
        Some(error) => Err(McpError::Rpc {
            code: error.code,
            message: error.message,
        }),
        None => Ok(inbound.result.unwrap_or(Value::Null)),
    };

    if let Some(tx) = pending.lock().await.remove(&id) {
        let _ = tx.send(outcome);
    }
}

async fn log_stderr(server_name: String, stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        debug!(server = %server_name, "{line}");
    }
}

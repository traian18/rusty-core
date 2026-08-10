//! A stdio MCP client: spawns a server process, drives the JSON-RPC
//! `initialize` handshake, and exposes `tools/list` + `tools/call`.
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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStderr, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::config::McpServerConfig;
use crate::protocol::{
    CallToolResult, InitializeResult, JsonRpcInbound, JsonRpcNotification, JsonRpcRequest,
    ListToolsResult, McpToolInfo, ServerInfo, MCP_PROTOCOL_VERSION,
};

const CLIENT_NAME: &str = "rusty-core";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Errors talking to an MCP server. All of these are handled as a logical
/// tool failure (`ToolResult { is_error: true }`) by
/// [`crate::tool::McpToolExecutor`] rather than aborting the run — the
/// model can see "the MCP server timed out" and retry or route around it.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("failed to spawn MCP server '{name}': {source}")]
    Spawn {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP server closed its connection")]
    Closed,
    #[error("I/O error talking to MCP server: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON from MCP server: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("MCP request timed out after {0:?}")]
    Timeout(Duration),
    #[error("MCP server returned an error (code {code}): {message}")]
    Rpc { code: i64, message: String },
}

type PendingMap = Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>;

/// A live connection to one MCP server, reachable over stdio.
///
/// Cheap to clone via `Arc` — every [`crate::tool::McpToolExecutor`]
/// discovered from the same server shares one `McpClient`, so a server
/// with ten tools spawns one process, not ten.
pub struct McpClient {
    stdin: Mutex<ChildStdin>,
    pending: Arc<PendingMap>,
    next_id: AtomicU64,
    child: Mutex<Child>,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    server_info: OnceLock<ServerInfo>,
    request_timeout: Duration,
}

impl McpClient {
    /// Spawn `config.command`, perform the `initialize` handshake, and send
    /// `notifications/initialized`. The returned client is ready for
    /// `list_tools`/`call_tool`.
    pub async fn connect(config: &McpServerConfig) -> Result<Arc<Self>, McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|source| McpError::Spawn {
            name: config.name.clone(),
            source,
        })?;
        let stdin = child.stdin.take().expect("stdin piped at spawn");
        let stdout = child.stdout.take().expect("stdout piped at spawn");
        let stderr = child.stderr.take().expect("stderr piped at spawn");

        let pending: Arc<PendingMap> = Arc::new(Mutex::new(HashMap::new()));
        let server_name = config.name.clone();

        let stderr_task = tokio::spawn(log_stderr(server_name.clone(), stderr));
        let reader_task = tokio::spawn(read_loop(server_name.clone(), stdout, pending.clone()));

        let client = Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            reader_task,
            stderr_task,
            server_info: OnceLock::new(),
            request_timeout: config.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
        });

        client.initialize().await?;
        Ok(client)
    }

    /// The server's self-reported name/version, if it sent one during
    /// `initialize`.
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.get()
    }

    async fn initialize(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION },
        });
        let result = self.request("initialize", Some(params)).await?;
        let result: InitializeResult = serde_json::from_value(result)?;
        if let Some(info) = result.server_info {
            let _ = self.server_info.set(info);
        }
        self.notify("notifications/initialized", None).await
    }

    /// List every tool the server advertises, following `nextCursor`
    /// pagination until exhausted.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
            let result = self.request("tools/list", params).await?;
            let result: ListToolsResult = serde_json::from_value(result)?;
            tools.extend(result.tools);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Invoke a remote tool by its server-local name.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult, McpError> {
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.request("tools/call", Some(params)).await?;
        Ok(serde_json::from_value(result)?)
    }

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

    async fn write_line(&self, line: &str) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Abort the reader/stderr tasks and kill the server process. Safe to
    /// call more than once.
    pub async fn shutdown(&self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        let _ = self.child.lock().await.kill().await;
    }
}

impl Drop for McpClient {
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

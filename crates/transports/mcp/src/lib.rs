#![warn(clippy::all)]

//! MCP **server** transport: exposes a running harness as an MCP server, so
//! Claude Desktop, Cursor, or VS Code can drive real sessions without any
//! harness-specific SDK.
//!
//! This is the mirror of `harness-tool-mcp`, which lets the harness *consume*
//! other MCP servers. Together they close both directions.
//!
//! # Why this sits on `RpcHandler`
//!
//! Like `harness-transport-{ipc,stdio,websocket}`, this crate serves an
//! [`RpcHandler`] rather than reaching for `harness_engine::Harness`
//! directly. That buys three things:
//!
//! - it depends only on `harness-protocol` + `harness-runtime`, so the
//!   transport layer stays uniform;
//! - `harnessd` gains MCP server mode by adding one flag next to the three
//!   it already has;
//! - sessions created over MCP flow through the same typed contract,
//!   admission cache, and revision tracking as every other client, instead
//!   of forking that machinery.
//!
//! # Stdout ownership
//!
//! Once [`serve`] is running, stdout is reserved **exclusively** for the MCP
//! stream — a stray `println!`, panic hook, or default logger corrupts the
//! framing. Route all logging to stderr before calling this. The same
//! warning applies to `harness-transport-stdio`, and the two cannot both
//! serve the real stdio pair in one process.
//!
//! # Scope
//!
//! `initialize`, `tools/list`, `tools/call`, `resources/list`,
//! `resources/read`. Sampling, roots, and prompts aren't implemented.

mod run;
mod tools;
mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use harness_protocol::mcp::MCP_PROTOCOL_VERSION;
use harness_protocol::rpc::{RpcRequestBody, RpcResponseBody};
use harness_protocol::skills::SkillsSpec;
use harness_protocol::tools::AgentToolset;
use harness_runtime::rpc::RpcHandler;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const SERVER_NAME: &str = "rusty-core";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PROMPT_TIMEOUT: Duration = Duration::from_secs(600);

/// What an MCP client cannot tell us, and so must be configured here.
#[derive(Debug, Clone)]
pub struct McpServeConfig {
    /// Integration id every MCP-created session uses (e.g. `"anthropic"`).
    pub integration: String,
    /// Provider-specific configuration for that integration.
    pub integration_config: Value,
    /// Workspace root for MCP-created sessions. Fixed by configuration
    /// rather than taken from the tool call, so a connected IDE cannot
    /// point the agent at an arbitrary directory.
    pub workspace_root: PathBuf,
    /// Toolset granted to MCP-created sessions.
    ///
    /// **Should be all-`Allow`.** There is no MCP-side channel to answer a
    /// permission prompt, so an `Ask` policy parks the run until it times
    /// out. `harness_prompt` detects that case and reports it rather than
    /// hanging silently, but the real fix is not to configure it that way.
    pub toolset: AgentToolset,
    /// Skills for MCP-created sessions, if any.
    pub skills: Option<SkillsSpec>,
    /// How long `harness_prompt` waits for a run before giving up.
    pub prompt_timeout: Duration,
}

impl McpServeConfig {
    pub fn new(integration: impl Into<String>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            integration: integration.into(),
            integration_config: json!({}),
            workspace_root: workspace_root.into(),
            toolset: AgentToolset {
                tools: Default::default(),
            },
            skills: None,
            prompt_timeout: DEFAULT_PROMPT_TIMEOUT,
        }
    }

    pub fn integration_config(mut self, config: Value) -> Self {
        self.integration_config = config;
        self
    }

    pub fn toolset(mut self, toolset: AgentToolset) -> Self {
        self.toolset = toolset;
        self
    }

    pub fn skills(mut self, skills: SkillsSpec) -> Self {
        self.skills = Some(skills);
        self
    }

    pub fn prompt_timeout(mut self, timeout: Duration) -> Self {
        self.prompt_timeout = timeout;
        self
    }
}

/// Serves the process's real stdin/stdout as an MCP server until `shutdown`
/// fires.
pub async fn serve(
    handler: Arc<dyn RpcHandler>,
    config: McpServeConfig,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    serve_io(
        tokio::io::stdin(),
        tokio::io::stdout(),
        handler,
        config,
        shutdown,
    )
    .await
}

/// Serves an arbitrary reader/writer pair.
///
/// Split out from [`serve`] for the same reason `harness-transport-stdio`
/// splits its own: tests drive this over `tokio::io::duplex()` instead of
/// spawning a subprocess.
pub async fn serve_io<R, W>(
    reader: R,
    writer: W,
    handler: Arc<dyn RpcHandler>,
    config: McpServeConfig,
    shutdown: CancellationToken,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    // Cancels in-flight tool calls when the connection ends. Deliberately
    // *not* wired to the writer — see below.
    let conn_cancel = CancellationToken::new();

    // The writer task owns the writer exclusively, so two concurrent
    // responses can never interleave mid-line.
    //
    // It exits only when the channel closes, never on a cancellation
    // signal. Racing a cancel against the queue drops replies that were
    // already queued: a peer that pipes several requests and closes stdin
    // would get answers to the first few and silence for the rest. Closing
    // the channel makes `recv()` return `None` *after* the backlog drains,
    // which is the ordering guarantee actually needed here. A dead pipe
    // still ends the loop, via the write error.
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(text) = out_rx.recv().await {
            if writer.write_all(text.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
                || writer.flush().await.is_err()
            {
                break;
            }
        }
    });

    let mut lines = BufReader::new(reader).lines();
    let result = read_loop(
        &mut lines,
        &handler,
        &config,
        &out_tx,
        &conn_cancel,
        &shutdown,
    )
    .await;

    conn_cancel.cancel();
    // Dropping the sender is what tells the writer to finish: it drains
    // whatever is queued, then sees the channel close and exits.
    drop(out_tx);
    let _ = writer_task.await;
    result
}

async fn read_loop<R>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    handler: &Arc<dyn RpcHandler>,
    config: &McpServeConfig,
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

                let incoming: wire::Incoming = match serde_json::from_str(&line) {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        // Malformed input still deserves a well-formed
                        // reply, with a null id since we couldn't read one.
                        send(out_tx, wire::Response::error(
                            Value::Null,
                            wire::PARSE_ERROR,
                            format!("could not parse request: {error}"),
                        )).await;
                        continue;
                    }
                };

                // Notifications are answered with silence, per JSON-RPC.
                if incoming.is_notification() {
                    debug!(method = %incoming.method, "MCP server: notification");
                    continue;
                }
                let id = incoming.id.clone().unwrap_or(Value::Null);

                let response = dispatch(handler, config, &incoming, conn_cancel).await;
                send(out_tx, match response {
                    Ok(result) => wire::Response::ok(id, result),
                    Err((code, message)) => wire::Response::error(id, code, message),
                }).await;
            }
        }
    }
}

type DispatchError = (i64, String);

async fn dispatch(
    handler: &Arc<dyn RpcHandler>,
    config: &McpServeConfig,
    incoming: &wire::Incoming,
    cancel: &CancellationToken,
) -> Result<Value, DispatchError> {
    let params = incoming.params();
    match incoming.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            // Only what is actually implemented — advertising `prompts` or
            // `sampling` here would make clients call methods that answer
            // "method not found".
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::tool_definitions()),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Err((wire::INVALID_PARAMS, "name is required".to_owned()));
            };
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(tools::call(handler, config, name, &arguments, cancel).await)
        }
        "resources/list" => Ok(list_resources(handler).await),
        "resources/read" => read_resource(handler, &params).await,
        other => Err((
            wire::METHOD_NOT_FOUND,
            format!("method {other:?} is not supported by this server"),
        )),
    }
}

/// Enumerates one resource per known session.
async fn list_resources(handler: &Arc<dyn RpcHandler>) -> Value {
    let sessions = match handler.handle(None, RpcRequestBody::ListSessions).await {
        RpcResponseBody::SessionsListed { sessions } => sessions,
        other => {
            warn!(?other, "MCP server: could not list sessions for resources");
            Vec::new()
        }
    };

    let resources: Vec<Value> = sessions
        .into_iter()
        .map(|session| {
            json!({
                "uri": format!("harness://session/{}", session.session_id),
                "name": session.title,
                "description": format!("Transcript of session {}", session.session_id),
                "mimeType": "text/plain",
            })
        })
        .collect();
    json!({ "resources": resources })
}

async fn read_resource(
    handler: &Arc<dyn RpcHandler>,
    params: &Value,
) -> Result<Value, DispatchError> {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return Err((wire::INVALID_PARAMS, "uri is required".to_owned()));
    };
    let Some(raw) = uri.strip_prefix("harness://session/") else {
        return Err((
            wire::INVALID_PARAMS,
            format!("unsupported resource uri {uri:?}"),
        ));
    };
    let Ok(session_id) = raw.parse() else {
        return Err((
            wire::INVALID_PARAMS,
            format!("{raw:?} is not a valid session id"),
        ));
    };

    let text = run::render_transcript(handler, session_id).await;
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/plain",
            "text": text,
        }]
    }))
}

/// Returns false once the peer is gone, so callers can stop early.
async fn send(out_tx: &mpsc::Sender<String>, response: wire::Response) -> bool {
    match serde_json::to_string(&response) {
        Ok(line) => out_tx.send(line).await.is_ok(),
        Err(error) => {
            // Serializing our own response type should be infallible; if it
            // ever isn't, losing one reply beats killing the connection.
            warn!(%error, "MCP server: could not serialize a response");
            false
        }
    }
}

/// Re-exported so `harnessd` can build a config without depending on
/// `harness-protocol` types it doesn't otherwise use.
pub use harness_protocol::tools::AgentToolset as ServeToolset;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_defaults_to_a_generous_prompt_timeout() {
        let config = McpServeConfig::new("anthropic", "/tmp/ws");
        assert_eq!(config.prompt_timeout, DEFAULT_PROMPT_TIMEOUT);
        assert_eq!(config.integration, "anthropic");
        assert!(config.skills.is_none());
    }

    #[test]
    fn builders_override_the_defaults() {
        let config = McpServeConfig::new("openai", "/tmp/ws")
            .integration_config(json!({"default_model": "gpt-test"}))
            .prompt_timeout(Duration::from_secs(5));
        assert_eq!(config.integration_config["default_model"], "gpt-test");
        assert_eq!(config.prompt_timeout, Duration::from_secs(5));
    }
}

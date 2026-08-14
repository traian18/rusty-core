#![warn(clippy::all)]

//! `harnessd`: exposes a running [`Harness`] over one or more transports so
//! an external process (an IDE) can drive sessions without linking Rust.
//!
//! Logging is routed to stderr unconditionally — the `--stdio` transport
//! reserves stdout exclusively for its newline-delimited RPC stream, and a
//! stray log line there would corrupt that framing.

mod handler;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;

use harness_engine::Harness;
use harness_integration_anthropic::AnthropicFactory;
use harness_integration_claude_code::ClaudeCodeFactory;
use harness_integration_codex::CodexFactory;
use harness_integration_gemini::GeminiFactory;
use harness_integration_openai::OpenAiFactory;
use harness_integration_openai_compatible::OpenAiCompatibleFactory;
use harness_runtime::rpc::RpcHandler;
use harness_session_store::JsonlSessionStore;
use tokio_util::sync::CancellationToken;

use handler::HarnessRpcHandler;

#[derive(Parser)]
#[command(
    name = "harnessd",
    about = "Harness daemon: exposes a running harness over one or more transports"
)]
struct Args {
    /// Path to a Unix domain socket to bind the IPC transport on.
    ///
    /// At least one transport flag is required; any combination of
    /// `--unix-socket`, `--tcp`, and `--stdio` may be given together.
    #[arg(long)]
    unix_socket: Option<PathBuf>,

    /// Address to bind the WebSocket transport on, e.g. `127.0.0.1:8787`.
    ///
    /// Loopback-only, unauthenticated in this version — see the security
    /// note in crates/transports/websocket/src/lib.rs before binding this to
    /// anything other than `127.0.0.1`.
    #[arg(long)]
    tcp: Option<SocketAddr>,

    /// Serve the stdio transport (newline-delimited JSON over stdin/stdout).
    /// Suited to an IDE that spawns harnessd as a child process.
    #[arg(long)]
    stdio: bool,

    /// Directory for session persistence (JsonlSessionStore).
    /// Defaults to `<cwd>/.harness/sessions`.
    #[arg(long)]
    sessions_dir: Option<PathBuf>,

    /// Serve as an **MCP server** over stdin/stdout, so Claude Desktop,
    /// Cursor, or VS Code can drive sessions without a harness-specific
    /// SDK. Mutually exclusive with `--stdio`: both reserve stdout.
    ///
    /// An MCP client can't know which provider this daemon is wired to, so
    /// `--mcp-integration` and `--mcp-workspace-root` supply what it can't.
    #[arg(long, conflicts_with = "stdio")]
    mcp_stdio: bool,

    /// Integration id for sessions created over MCP server mode.
    #[arg(long, default_value = "anthropic", requires = "mcp_stdio")]
    mcp_integration: String,

    /// Raw JSON config for `--mcp-integration`.
    #[arg(long, default_value = "{}", requires = "mcp_stdio")]
    mcp_integration_config: String,

    /// Workspace root for sessions created over MCP server mode. Defaults
    /// to the current directory. Fixed here rather than taken from the tool
    /// call, so a connected IDE can't point the agent at an arbitrary
    /// directory.
    #[arg(long, requires = "mcp_stdio")]
    mcp_workspace_root: Option<PathBuf>,
}

/// Tools granted to MCP-created sessions.
///
/// Every tool is `Allow`: MCP has no channel for answering a permission
/// prompt, so an `Ask` policy would park the run until it timed out.
/// `harness_prompt` detects and reports that case rather than hanging, but
/// the correct configuration is simply not to create it.
fn mcp_toolset() -> harness_protocol::tools::AgentToolset {
    use harness_protocol::ids::ToolId;
    use harness_protocol::tools::{
        AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy,
    };

    let mut tools = std::collections::HashMap::new();
    for (name, description) in [
        ("fs.read", "Read a file from the workspace."),
        ("fs.edit", "Create or replace a workspace file."),
        ("workspace.search", "Search workspace files."),
        ("shell.exec", "Run a shell command in the workspace."),
        ("git.status", "Show git status."),
        ("git.diff", "Show a git diff."),
        ("git.log", "Show git history."),
        ("git.show", "Show a git object."),
    ] {
        let id = ToolId::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.to_owned(),
                    description: description.to_owned(),
                    input_schema: serde_json::json!({ "type": "object" }),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            },
        );
    }
    AgentToolset { tools }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.unix_socket.is_none() && args.tcp.is_none() && !args.stdio && !args.mcp_stdio {
        bail!(
            "at least one transport must be selected; pass --unix-socket <path>, --tcp <addr>, --stdio, and/or --mcp-stdio"
        );
    }

    let sessions_dir = args
        .sessions_dir
        .clone()
        .unwrap_or_else(default_sessions_dir);
    std::fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("creating sessions dir {}", sessions_dir.display()))?;

    let harness = Harness::builder()
        .register_integration(Arc::new(AnthropicFactory))
        .register_integration(Arc::new(OpenAiFactory))
        .register_integration(Arc::new(OpenAiCompatibleFactory))
        .register_integration(Arc::new(GeminiFactory))
        .register_integration(Arc::new(ClaudeCodeFactory))
        .register_integration(Arc::new(CodexFactory))
        .session_store(Arc::new(JsonlSessionStore::new(sessions_dir)))
        .build()
        .await
        .context("building Harness")?;

    // M6: install the process-wide Prometheus recorder once, here, at
    // startup. Everywhere else in the workspace only depends on the
    // lightweight `metrics` facade crate and calls `metrics::counter!`/
    // `histogram!`/`gauge!` — none of it needs to know a Prometheus
    // recorder specifically exists, only this binary does. The resulting
    // handle is rendered on demand by the `GetDiagnostics` RPC rather than
    // served over its own HTTP listener — see that RPC variant's doc
    // comment for why.
    let metrics_handle = match metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(%error, "failed to install the Prometheus metrics recorder; GetDiagnostics will report empty metrics text");
            None
        }
    };

    let harness = Arc::new(harness);
    let handler: Arc<dyn RpcHandler> = Arc::new(HarnessRpcHandler::new_with_metrics(
        harness.clone(),
        metrics_handle,
    ));

    let shutdown = CancellationToken::new();
    spawn_shutdown_listener(shutdown.clone());

    let mut tasks = Vec::new();

    if let Some(socket_path) = args.unix_socket {
        let handler = handler.clone();
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(error) = harness_transport_ipc::serve(&socket_path, handler, shutdown).await
            {
                tracing::error!(%error, "ipc transport exited with an error");
            }
        }));
    }

    if let Some(addr) = args.tcp {
        let handler = handler.clone();
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(error) = harness_transport_websocket::serve(addr, handler, shutdown).await {
                tracing::error!(%error, "websocket transport exited with an error");
            }
        }));
    }

    if args.stdio {
        let handler = handler.clone();
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(error) = harness_transport_stdio::serve(handler, shutdown).await {
                tracing::error!(%error, "stdio transport exited with an error");
            }
        }));
    }

    if args.mcp_stdio {
        let integration_config: serde_json::Value =
            serde_json::from_str(&args.mcp_integration_config)
                .context("--mcp-integration-config must be valid JSON")?;
        let workspace_root = match args.mcp_workspace_root {
            Some(root) => root,
            None => std::env::current_dir().context("resolving the MCP workspace root")?,
        };
        let config = harness_transport_mcp::McpServeConfig::new(
            args.mcp_integration.clone(),
            workspace_root,
        )
        .integration_config(integration_config)
        .toolset(mcp_toolset());

        let handler = handler.clone();
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(error) =
                harness_transport_mcp::serve(handler, config, shutdown.clone()).await
            {
                tracing::error!(%error, "mcp server transport exited with an error");
            }
            // MCP server mode is spawned as a subprocess by an IDE, and the
            // IDE ends it by closing stdin. `serve` returns on that EOF, so
            // this is the signal to shut the daemon down — without it the
            // process would sit in the shutdown wait below forever and every
            // client restart would leak a harnessd.
            tracing::info!("mcp server transport finished; shutting down");
            shutdown.cancel();
        }));
    }

    // E3: previously nothing walked active sessions on shutdown at all —
    // `spawn_shutdown_listener` only cancelled the token that stops
    // transports from accepting new work, leaving whatever sessions were
    // active at that instant torn down without their final checkpoint ever
    // being committed. Wait for the same shutdown signal here, then drain
    // every active session through `close_session` (checkpoint-then-
    // shutdown, exactly once — see `SessionManager::close_all_sessions`)
    // before the transports are allowed to finish exiting, bounded by a
    // grace period so one stuck session can't hang the whole process.
    shutdown.cancelled().await;
    let drain_grace_period = Duration::from_secs(10);
    let unclosed = harness
        .session_manager()
        .close_all_sessions(drain_grace_period)
        .await;
    if !unclosed.is_empty() {
        tracing::warn!(
            count = unclosed.len(),
            ?drain_grace_period,
            "some sessions did not close cleanly during the shutdown drain; their last \
             checkpoint before this point remains durable, but any work in flight at \
             shutdown time may not be"
        );
    }

    for task in tasks {
        let _ = task.await;
    }

    Ok(())
}

/// Cancels `shutdown` on Ctrl-C or SIGTERM so every running transport's
/// accept loop stops and in-flight connections wind down.
fn spawn_shutdown_listener(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("installing SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        tracing::info!("shutdown signal received");
        shutdown.cancel();
    });
}

fn default_sessions_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".harness")
        .join("sessions")
}

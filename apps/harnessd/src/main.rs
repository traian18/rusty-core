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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.unix_socket.is_none() && args.tcp.is_none() && !args.stdio {
        bail!(
            "at least one transport must be selected; pass --unix-socket <path>, --tcp <addr>, and/or --stdio"
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

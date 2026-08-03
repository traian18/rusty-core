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

use anyhow::{bail, Context, Result};
use clap::Parser;

use harness_engine::Harness;
use harness_integration_anthropic::AnthropicFactory;
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

    let sessions_dir = args.sessions_dir.clone().unwrap_or_else(default_sessions_dir);
    std::fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("creating sessions dir {}", sessions_dir.display()))?;

    let harness = Harness::builder()
        .register_integration(Arc::new(AnthropicFactory))
        .session_store(Arc::new(JsonlSessionStore::new(sessions_dir)))
        .build()
        .await
        .context("building Harness")?;

    let handler: Arc<dyn RpcHandler> = Arc::new(HarnessRpcHandler::new(Arc::new(harness)));

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
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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

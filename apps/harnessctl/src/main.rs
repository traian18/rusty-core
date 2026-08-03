#![warn(clippy::all)]

//! `harnessctl`: a reference CLI client for a running `harnessd` over the
//! IPC transport. Doubles as an operational tool for debugging/scripting and
//! as a worked example for anyone writing a real IDE-side client against the
//! same wire protocol (see `harness_protocol::rpc`).

mod client;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use harness_protocol::commands::{PermissionDecision, UserInput};
use harness_protocol::ids::{PermissionId, SessionId};
use harness_protocol::rpc::{RpcRequestBody, RpcResponseBody};
use harness_protocol::tools::AgentToolset;

use client::HarnessClient;

#[derive(Parser)]
#[command(
    name = "harnessctl",
    about = "Reference CLI client for a running harnessd (IPC transport)"
)]
struct Cli {
    /// Path to the harnessd Unix domain socket.
    #[arg(long)]
    socket: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Session lifecycle and interaction commands.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Resolve a pending permission request.
    Permission {
        #[command(subcommand)]
        command: PermissionCommand,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Create a new session against a workspace directory.
    Create {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        integration: String,
        /// Raw JSON config for the integration. Defaults to `{}`, which uses
        /// the integration's environment-variable-based defaults (e.g.
        /// `ANTHROPIC_API_KEY` for `--integration anthropic`).
        #[arg(long, default_value = "{}")]
        config_json: String,
    },
    /// Send a prompt to a session.
    Send { session_id: String, prompt: String },
    /// Stream a session's events to stdout until Ctrl-C.
    Events {
        session_id: String,
        /// Print raw JSON instead of a compact human-readable line.
        #[arg(long)]
        json: bool,
    },
    /// Print a session's current snapshot.
    Snapshot { session_id: String },
    /// Cancel a session's active run.
    Cancel { session_id: String },
    /// Close a session.
    Close { session_id: String },
}

#[derive(Subcommand)]
enum PermissionCommand {
    /// Approve or deny a pending permission request.
    Resolve {
        session_id: String,
        permission_id: String,
        decision: DecisionArg,
    },
}

#[derive(Clone, ValueEnum)]
enum DecisionArg {
    Approve,
    Deny,
}

impl From<DecisionArg> for PermissionDecision {
    fn from(value: DecisionArg) -> Self {
        match value {
            DecisionArg::Approve => PermissionDecision::Approved,
            DecisionArg::Deny => PermissionDecision::Denied,
        }
    }
}

fn parse_session_id(raw: &str) -> Result<SessionId> {
    SessionId::from_str(raw).with_context(|| format!("invalid session id: {raw}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut client = HarnessClient::connect(&cli.socket).await?;

    match cli.command {
        Command::Session { command } => run_session_command(&mut client, command).await,
        Command::Permission { command } => run_permission_command(&mut client, command).await,
    }
}

async fn run_session_command(client: &mut HarnessClient, command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Create {
            workspace,
            integration,
            config_json,
        } => {
            let integration_config: serde_json::Value = serde_json::from_str(&config_json)
                .context("--config-json must be valid JSON")?;
            let response = client
                .request(
                    None,
                    RpcRequestBody::CreateSession {
                        workspace_root: workspace,
                        integration,
                        integration_config,
                        toolset: AgentToolset {
                            tools: HashMap::new(),
                        },
                    },
                )
                .await?;
            match response {
                RpcResponseBody::SessionCreated { session_id } => {
                    println!("{session_id}");
                    Ok(())
                }
                other => Err(anyhow::anyhow!("create session failed: {other:?}")),
            }
        }

        SessionCommand::Send { session_id, prompt } => {
            let session_id = parse_session_id(&session_id)?;
            let response = client
                .request(
                    Some(session_id),
                    RpcRequestBody::Prompt(UserInput {
                        text: prompt,
                        attachments: vec![],
                    }),
                )
                .await?;
            print_ack_or_error(response)
        }

        SessionCommand::Events { session_id, json } => {
            let session_id = parse_session_id(&session_id)?;
            client.subscribe(session_id).await?;
            loop {
                match client.next_event().await? {
                    Some(RpcResponseBody::Event(envelope)) => {
                        if json {
                            println!("{}", serde_json::to_string(&envelope)?);
                        } else {
                            println!("[{}] {:?}", envelope.agent_sequence, envelope.event);
                        }
                    }
                    Some(other) => {
                        // Not expected once subscribed, but don't crash on it.
                        eprintln!("unexpected frame while subscribed: {other:?}");
                    }
                    None => {
                        println!("(connection closed)");
                        break;
                    }
                }
            }
            Ok(())
        }

        SessionCommand::Snapshot { session_id } => {
            let session_id = parse_session_id(&session_id)?;
            let response = client
                .request(Some(session_id), RpcRequestBody::Snapshot)
                .await?;
            match response {
                RpcResponseBody::Snapshot(snapshot) => {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                    Ok(())
                }
                other => Err(anyhow::anyhow!("snapshot failed: {other:?}")),
            }
        }

        SessionCommand::Cancel { session_id } => {
            let session_id = parse_session_id(&session_id)?;
            let response = client
                .request(Some(session_id), RpcRequestBody::Cancel)
                .await?;
            print_ack_or_error(response)
        }

        SessionCommand::Close { session_id } => {
            let session_id = parse_session_id(&session_id)?;
            let response = client
                .request(Some(session_id), RpcRequestBody::CloseSession)
                .await?;
            print_ack_or_error(response)
        }
    }
}

async fn run_permission_command(
    client: &mut HarnessClient,
    command: PermissionCommand,
) -> Result<()> {
    match command {
        PermissionCommand::Resolve {
            session_id,
            permission_id,
            decision,
        } => {
            let session_id = parse_session_id(&session_id)?;
            let id = PermissionId::from_str(&permission_id)
                .with_context(|| format!("invalid permission id: {permission_id}"))?;
            let response = client
                .request(
                    Some(session_id),
                    RpcRequestBody::ResolvePermission {
                        id,
                        decision: decision.into(),
                    },
                )
                .await?;
            print_ack_or_error(response)
        }
    }
}

fn print_ack_or_error(response: RpcResponseBody) -> Result<()> {
    match response {
        RpcResponseBody::Ack => {
            println!("ok");
            Ok(())
        }
        other => Err(anyhow::anyhow!("request failed: {other:?}")),
    }
}

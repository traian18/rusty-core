#![warn(clippy::all)]

//! `harnessctl`: a reference CLI client for a running `harnessd` over the
//! IPC transport. Doubles as an operational tool for debugging/scripting and
//! as a worked example for anyone writing a real IDE-side client against the
//! same wire protocol (see `harness_protocol::rpc`).

mod chat;
mod client;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use harness_protocol::admission::{CommandId, MutationMetadata};
use harness_protocol::commands::{PermissionDecision, UserInput};
use harness_protocol::ids::{PermissionId, SessionId, ToolId};
use harness_protocol::rpc::{MutationCommand, RpcRequestBody, RpcResponseBody};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCapability, ToolDescriptor, ToolPolicy,
};

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
    /// Create a session and drop into an interactive TUI: type a prompt,
    /// watch it stream, approve/reject permission prompts inline. For
    /// manual testing — the CLI-scriptable equivalent is `session
    /// create`/`send`/`events`.
    Chat {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        integration: String,
        /// Raw JSON config for the integration. Defaults to `{}`, which uses
        /// the integration's environment-variable-based defaults (e.g.
        /// `ANTHROPIC_API_KEY` for `--integration anthropic`).
        #[arg(long, default_value = "{}")]
        config_json: String,
        /// Comma-separated tool names to enable — see `session create --help`
        /// for the known list.
        #[arg(long, value_delimiter = ',', conflicts_with = "all_tools")]
        tools: Vec<String>,
        /// Enable every known tool.
        #[arg(long, conflicts_with = "tools")]
        all_tools: bool,
        /// MCP server(s) to connect at session start, as
        /// `name=command[,arg1,arg2,...]` for a stdio server or
        /// `name=https://host/mcp` for an HTTP one. Repeatable. Discovered
        /// tools are registered as `mcp.<name>.<tool>`, alongside
        /// `--tools`. For env vars, a working directory, request headers,
        /// or a non-default timeout, construct
        /// `RpcRequestBody::CreateSession.mcp_servers` directly instead of
        /// this flag.
        #[arg(long = "mcp-server")]
        mcp_servers: Vec<String>,
        /// Enable filesystem skills. Scans `<workspace>/.harness/skills`
        /// plus any `--skills-dir`, registering `skill.load`/`skill.read`
        /// and adding each skill's one-line description to the system
        /// prompt. Off unless this or `--skills-dir` is given.
        #[arg(long)]
        skills: bool,
        /// Extra directory to scan for `SKILL.md` files. Repeatable, and
        /// implies `--skills`. Later directories win on a name collision.
        #[arg(long = "skills-dir")]
        skills_dirs: Vec<PathBuf>,
        /// Also scan the daemon operator's `$HOME/.harness/skills`. Off by
        /// default: over a socket the daemon's home directory is not the
        /// caller's, so loading it is an explicit choice.
        #[arg(long)]
        user_skills: bool,
    },
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
        /// Comma-separated tool names to enable, e.g.
        /// "fs.read,fs.edit,shell.exec". Known tools: fs.read, fs.edit,
        /// workspace.search, shell.exec, git.status, git.diff, git.log,
        /// git.show, web.fetch. Omit for a toolless session (raw model
        /// plumbing only).
        #[arg(long, value_delimiter = ',', conflicts_with = "all_tools")]
        tools: Vec<String>,
        /// Enable every known tool — shorthand for `--tools` with the full
        /// list above.
        #[arg(long, conflicts_with = "tools")]
        all_tools: bool,
        /// MCP server(s) to connect at session start, as
        /// `name=command[,arg1,arg2,...]` for a stdio server or
        /// `name=https://host/mcp` for an HTTP one. Repeatable. Discovered
        /// tools are registered as `mcp.<name>.<tool>`, alongside
        /// `--tools`. For env vars, a working directory, request headers,
        /// or a non-default timeout, construct
        /// `RpcRequestBody::CreateSession.mcp_servers` directly instead of
        /// this flag.
        #[arg(long = "mcp-server")]
        mcp_servers: Vec<String>,
        /// Enable filesystem skills. Scans `<workspace>/.harness/skills`
        /// plus any `--skills-dir`, registering `skill.load`/`skill.read`
        /// and adding each skill's one-line description to the system
        /// prompt. Off unless this or `--skills-dir` is given.
        #[arg(long)]
        skills: bool,
        /// Extra directory to scan for `SKILL.md` files. Repeatable, and
        /// implies `--skills`. Later directories win on a name collision.
        #[arg(long = "skills-dir")]
        skills_dirs: Vec<PathBuf>,
        /// Also scan the daemon operator's `$HOME/.harness/skills`. Off by
        /// default: over a socket the daemon's home directory is not the
        /// caller's, so loading it is an explicit choice.
        #[arg(long)]
        user_skills: bool,
    },
    /// Send a prompt to a session.
    Send { session_id: String, prompt: String },
    /// Stream a session's events to stdout until Ctrl-C.
    Events {
        session_id: String,
        /// Print raw JSON instead of a compact human-readable line.
        #[arg(long)]
        json: bool,
        /// Resume from a prior subscription: replay every durable event with
        /// `session_sequence` greater than this value before live events
        /// begin. Pass the highest `session_sequence` seen before a
        /// disconnect to reconnect without gaps or duplicates. Omit for a
        /// fresh, live-only subscription.
        #[arg(long)]
        since_seq: Option<u64>,
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

/// Every tool `SessionBuilder::build_executor_for`
/// (crates/harness-engine/src/session_builder.rs) knows how to construct,
/// with the description and default permission used when building the
/// toolset from `--tools`/`--all-tools`. Kept in one place so this list and
/// that match statement don't silently drift apart.
fn known_tool_specs() -> Vec<(&'static str, &'static str, PermissionMode)> {
    vec![
        (
            "fs.read",
            "Read a file from the workspace.",
            PermissionMode::Allow,
        ),
        ("fs.edit", "Replace a workspace file.", PermissionMode::Ask),
        (
            "workspace.search",
            "Search workspace files.",
            PermissionMode::Allow,
        ),
        ("shell.exec", "Run a shell command.", PermissionMode::Ask),
        (
            "git.status",
            "Show working-tree and index status for changed files.",
            PermissionMode::Allow,
        ),
        (
            "git.diff",
            "Show a diff for a path or the whole tree (working tree or staged).",
            PermissionMode::Allow,
        ),
        (
            "git.log",
            "List recent commits, optionally filtered to those touching a path.",
            PermissionMode::Allow,
        ),
        (
            "git.show",
            "Show a single commit's metadata and diff by ref/SHA.",
            PermissionMode::Allow,
        ),
        (
            "web.fetch",
            "Fetch a URL over HTTP(S) and return its text content.",
            PermissionMode::Ask,
        ),
    ]
}

/// Resolves the `--tools`/`--all-tools` pair (shared by `session create` and
/// `chat`) into a concrete list of tool names.
fn resolve_tool_names(tools: Vec<String>, all_tools: bool) -> Vec<String> {
    if all_tools {
        known_tool_specs()
            .into_iter()
            .map(|(name, _, _)| name.to_string())
            .collect()
    } else {
        tools
    }
}

/// Parses one `--mcp-server` value: `name=command[,arg1,arg2,...]`. Covers
/// the common case (a named server plus its launch command/args); env
/// vars, a working directory, and a non-default timeout aren't
/// expressible through this flag — construct `McpServerSpec` directly
/// (over the wire, or via `harness-engine`'s `SessionBuilder::mcp_server`
/// when embedding) for those.
/// Parses one `--mcp-server` value.
///
/// Two forms, told apart by whether the part after `=` looks like a URL:
///
/// - `name=command[,arg1,arg2,...]` — a stdio server
/// - `name=http://host/mcp` or `name=https://host/mcp` — an HTTP server
///
/// A URL is never a plausible executable name, so the discrimination is
/// unambiguous and needs no extra flag.
fn parse_mcp_server_spec(raw: &str) -> Result<harness_protocol::mcp::McpServerSpec> {
    use harness_protocol::mcp::{McpServerSpec, McpTransportSpec};

    let (name, rest) = raw.split_once('=').with_context(|| {
        format!("--mcp-server value {raw:?} must be name=command[,arg,...] or name=URL")
    })?;
    if name.is_empty() {
        anyhow::bail!("--mcp-server value {raw:?} has an empty name before '='");
    }

    if rest.starts_with("http://") || rest.starts_with("https://") {
        return Ok(McpServerSpec::http(name, rest));
    }

    let mut parts = rest.split(',');
    let command = parts
        .next()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("--mcp-server value {raw:?} is missing a command after '='"))?;

    let mut spec = McpServerSpec::stdio(name, command);
    spec.transport = Some(McpTransportSpec::Stdio {
        command: command.to_owned(),
        args: parts.map(str::to_owned).collect(),
        env: HashMap::new(),
        cwd: None,
    });
    Ok(spec)
}

fn parse_mcp_servers(raw: &[String]) -> Result<Vec<harness_protocol::mcp::McpServerSpec>> {
    raw.iter()
        .map(|value| parse_mcp_server_spec(value))
        .collect()
}

/// Builds the `CreateSession.skills` field from the three skill flags.
///
/// Returns `None` — skills disabled entirely — unless the caller asked for
/// them, so a client that never mentions skills gets exactly the behavior it
/// had before the flags existed. Naming a `--skills-dir` implies `--skills`,
/// since asking for a directory and then not scanning it would be absurd.
fn skills_spec(
    enabled: bool,
    dirs: Vec<PathBuf>,
    user: bool,
) -> Option<harness_protocol::skills::SkillsSpec> {
    if !enabled && dirs.is_empty() && !user {
        return None;
    }
    Some(harness_protocol::skills::SkillsSpec {
        include_user_dir: user,
        include_workspace_dir: true,
        roots: dirs,
    })
}

fn build_toolset(names: &[String]) -> Result<AgentToolset> {
    let known = known_tool_specs();
    let mut tools = HashMap::new();
    for name in names {
        let (name, description, permission) = known
            .iter()
            .find(|(known_name, _, _)| known_name == name)
            .cloned()
            .with_context(|| {
                let available: Vec<&str> = known.iter().map(|(n, _, _)| *n).collect();
                format!(
                    "unknown tool '{name}'; known tools: {}",
                    available.join(", ")
                )
            })?;
        let id = ToolId::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.to_string(),
                    description: description.to_string(),
                    input_schema: serde_json::json!({ "type": "object" }),
                },
                policy: ToolPolicy {
                    permission,
                    enabled: true,
                },
                delegatable: false,
            },
        );
    }
    Ok(AgentToolset { tools })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Chat {
            workspace,
            integration,
            config_json,
            tools,
            all_tools,
            mcp_servers,
            skills,
            skills_dirs,
            user_skills,
        } => {
            let integration_config: serde_json::Value =
                serde_json::from_str(&config_json).context("--config-json must be valid JSON")?;
            let toolset = build_toolset(&resolve_tool_names(tools, all_tools))?;
            let mcp_servers = parse_mcp_servers(&mcp_servers)?;
            chat::run(
                &cli.socket,
                workspace,
                integration,
                integration_config,
                toolset,
                mcp_servers,
                skills_spec(skills, skills_dirs, user_skills),
            )
            .await
        }
        Command::Session { command } => {
            let mut client = HarnessClient::connect(&cli.socket).await?;
            run_session_command(&mut client, command).await
        }
        Command::Permission { command } => {
            let mut client = HarnessClient::connect(&cli.socket).await?;
            run_permission_command(&mut client, command).await
        }
    }
}

async fn run_session_command(client: &mut HarnessClient, command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Create {
            workspace,
            integration,
            config_json,
            tools,
            all_tools,
            mcp_servers,
            skills,
            skills_dirs,
            user_skills,
        } => {
            let integration_config: serde_json::Value =
                serde_json::from_str(&config_json).context("--config-json must be valid JSON")?;
            let toolset = build_toolset(&resolve_tool_names(tools, all_tools))?;
            let mcp_servers = parse_mcp_servers(&mcp_servers)?;
            let response = client
                .request(
                    None,
                    RpcRequestBody::CreateSession {
                        workspace_root: workspace,
                        integration,
                        integration_config,
                        toolset,
                        mcp_servers,
                        skills: skills_spec(skills, skills_dirs, user_skills),
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
                    mutation(
                        session_id,
                        MutationCommand::Prompt(UserInput {
                            text: prompt,
                            attachments: vec![],
                        }),
                    ),
                )
                .await?;
            print_ack_or_error(response)
        }

        SessionCommand::Events {
            session_id,
            json,
            since_seq,
        } => {
            let session_id = parse_session_id(&session_id)?;
            client.subscribe(session_id, since_seq).await?;
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
                .request(
                    Some(session_id),
                    mutation(session_id, MutationCommand::Cancel),
                )
                .await?;
            print_ack_or_error(response)
        }

        SessionCommand::Close { session_id } => {
            let session_id = parse_session_id(&session_id)?;
            let response = client
                .request(
                    Some(session_id),
                    mutation(session_id, MutationCommand::CloseSession),
                )
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
                    mutation(
                        session_id,
                        MutationCommand::ResolvePermission {
                            id,
                            decision: decision.into(),
                        },
                    ),
                )
                .await?;
            print_ack_or_error(response)
        }
    }
}

fn mutation(session_id: SessionId, command: MutationCommand) -> RpcRequestBody {
    RpcRequestBody::Mutate {
        metadata: MutationMetadata {
            command_id: CommandId::new(),
            session_id,
            run_id: None,
            expected_session_revision: None,
            trace_id: None,
        },
        command,
    }
}

fn print_ack_or_error(response: RpcResponseBody) -> Result<()> {
    match response {
        RpcResponseBody::Ack | RpcResponseBody::Admission { .. } => {
            println!("ok");
            Ok(())
        }
        other => Err(anyhow::anyhow!("request failed: {other:?}")),
    }
}

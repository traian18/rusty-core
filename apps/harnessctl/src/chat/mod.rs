//! `harnessctl chat`: a small interactive ratatui TUI for manually testing a
//! running `harnessd` — creates a session, then gives you a live chat loop
//! (prompt in, streamed events out, inline permission approve/reject)
//! instead of composing separate `session send`/`events`/`snapshot` calls
//! by hand. Same keybindings and layout as `apps/harness`'s standalone TUI;
//! the difference is this one talks to a daemon over the wire instead of
//! running a session in-process.

pub mod input;
pub mod render;
pub mod state;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use rusty_protocol::admission::{CommandId, MutationMetadata};
use rusty_protocol::commands::{PermissionDecision, UserInput};
use rusty_protocol::ids::SessionId;
use rusty_protocol::rpc::{
    MutationCommand, RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody,
};
use rusty_protocol::tools::AgentToolset;
use rusty_transport_ipc::framing::{read_frame, write_frame};

use crate::client::HarnessClient;
use input::InputAction;
use state::ChatState;

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// What the chat session is created with.
pub struct ChatArgs {
    pub workspace: PathBuf,
    pub integration: String,
    pub integration_config: serde_json::Value,
    pub toolset: AgentToolset,
    pub mcp_servers: Vec<rusty_protocol::mcp::McpServerSpec>,
    pub skills: Option<rusty_protocol::skills::SkillsSpec>,
}

/// Creates a session against `socket`, subscribes to it, and runs the
/// interactive chat loop until the user quits.
pub async fn run(socket: &std::path::Path, args: ChatArgs) -> Result<()> {
    let (session_id, mut responses, mut writer) = open_session(socket, args).await?;
    let mut state = ChatState::new(session_id);
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    while !state.should_quit {
        fold_responses(&mut state, &mut responses);
        terminal.draw(|frame| render::render(frame, &state))?;
        if event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = event::read()? {
                handle_key(&mut state, &mut writer, input::map_key(key)).await?;
            }
        }
    }
    // Best-effort — the terminal is about to be restored either way.
    let _ = writer.mutate(MutationCommand::CloseSession).await;
    Ok(())
}

/// Connects, creates the session and subscribes. `HarnessClient::connect` performs the `Hello`
/// handshake first, so a version-mismatched daemon fails here with a clear error. From then on
/// reads and writes are concurrent: a background task owns the read half and forwards every frame
/// into the returned channel, which the render loop drains without blocking; the returned writer
/// owns the write half.
async fn open_session(
    socket: &std::path::Path,
    args: ChatArgs,
) -> Result<(SessionId, mpsc::UnboundedReceiver<RpcResponse>, Writer)> {
    let mut client = HarnessClient::connect(socket).await?;
    let created = client
        .request(
            None,
            RpcRequestBody::CreateSession {
                execution_policy: None,
                workspace_root: args.workspace,
                integration: args.integration,
                integration_config: args.integration_config,
                toolset: args.toolset,
                mcp_servers: args.mcp_servers,
                skills: args.skills,
            },
        )
        .await?;
    let session_id = match created {
        RpcResponseBody::SessionCreated { session_id } => session_id,
        other => anyhow::bail!("create session failed: {other:?}"),
    };
    // A freshly created session has no history to resume, so `since_seq` is `None`: a live-only
    // subscription. A reconnect flow would pass the highest `session_sequence` seen before.
    client.subscribe(session_id, None).await?;
    let (read_half, write_half) = client.into_stream().into_split();
    let (tx, rx) = mpsc::unbounded_channel::<RpcResponse>();
    tokio::spawn(forward_frames(read_half, tx));
    let writer = Writer {
        half: write_half,
        next_id: 1,
        session_id,
    };
    Ok((session_id, rx, writer))
}

/// Forwards every frame to the render loop until the connection or the receiver closes.
async fn forward_frames(mut read_half: OwnedReadHalf, tx: mpsc::UnboundedSender<RpcResponse>) {
    while let Ok(Some(bytes)) = read_frame(&mut read_half).await {
        let Ok(response) = serde_json::from_slice::<RpcResponse>(&bytes) else {
            continue;
        };
        if tx.send(response).is_err() {
            break;
        }
    }
}

/// Folds everything the daemon sent since the last frame into the chat state.
fn fold_responses(state: &mut ChatState, responses: &mut mpsc::UnboundedReceiver<RpcResponse>) {
    while let Ok(response) = responses.try_recv() {
        match response.body {
            RpcResponseBody::Event(envelope) => state.fold_event(envelope),
            RpcResponseBody::Failure(error) => {
                state.push_log(format!("● error: {}", error.message))
            }
            _ => {}
        }
    }
}

/// What one key press does.
async fn handle_key(state: &mut ChatState, writer: &mut Writer, action: InputAction) -> Result<()> {
    match action {
        InputAction::Insert(character) => state.input.push(character),
        InputAction::Backspace => {
            state.input.pop();
        }
        InputAction::Submit if !state.input.trim().is_empty() => {
            let prompt = std::mem::take(&mut state.input);
            state.push_log(format!("> {prompt}"));
            let input = UserInput {
                text: prompt,
                attachments: vec![],
            };
            writer.mutate(MutationCommand::Prompt(input)).await?;
        }
        InputAction::Cancel => {
            writer.mutate(MutationCommand::Cancel).await?;
            state.push_log("● cancellation requested".to_string());
        }
        InputAction::Approve if state.pending_permission.is_some() => {
            answer_permission(state, writer, PermissionDecision::Approved).await?;
            state.push_log("● permission approved".to_string());
        }
        InputAction::Reject if state.pending_permission.is_some() => {
            answer_permission(state, writer, PermissionDecision::Denied).await?;
            state.push_log("● permission rejected".to_string());
        }
        InputAction::Quit => state.should_quit = true,
        InputAction::Submit | InputAction::Approve | InputAction::Reject | InputAction::Ignore => {}
    }
    Ok(())
}

async fn answer_permission(
    state: &mut ChatState,
    writer: &mut Writer,
    decision: PermissionDecision,
) -> Result<()> {
    let Some(id) = state.pending_permission.take() else {
        return Ok(());
    };
    writer
        .mutate(MutationCommand::ResolvePermission { id, decision })
        .await
}

/// The write half of the connection and the request counter.
struct Writer {
    half: OwnedWriteHalf,
    next_id: u64,
    session_id: SessionId,
}

impl Writer {
    async fn mutate(&mut self, command: MutationCommand) -> Result<()> {
        let body = mutation(self.session_id, command);
        let request = RpcRequest {
            id: RequestCorrelationId(self.next_id),
            session_id: Some(self.session_id),
            body,
        };
        self.next_id += 1;
        write_frame(&mut self.half, &serde_json::to_vec(&request)?).await?;
        Ok(())
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

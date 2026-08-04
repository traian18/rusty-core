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
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use harness_protocol::commands::{PermissionDecision, UserInput};
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{
    RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody,
};
use harness_protocol::tools::AgentToolset;
use harness_transport_ipc::framing::{read_frame, write_frame};

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

/// Creates a session against `socket`, subscribes to it, and runs the
/// interactive chat loop until the user quits.
pub async fn run(
    socket: &std::path::Path,
    workspace: PathBuf,
    integration: String,
    integration_config: serde_json::Value,
    toolset: AgentToolset,
) -> Result<()> {
    let mut client = HarnessClient::connect(socket).await?;

    let session_id = match client
        .request(
            None,
            RpcRequestBody::CreateSession {
                workspace_root: workspace,
                integration,
                integration_config,
                toolset,
            },
        )
        .await?
    {
        RpcResponseBody::SessionCreated { session_id } => session_id,
        other => anyhow::bail!("create session failed: {other:?}"),
    };
    client.subscribe(session_id).await?;

    // From here on, reads and writes happen concurrently: a background task
    // owns the read half and forwards every frame (pushed events as well as
    // responses to requests sent below) into a channel the render loop
    // drains non-blockingly; the foreground loop owns the write half.
    let (read_half, mut write_half) = client.into_stream().into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<RpcResponse>();
    tokio::spawn(async move {
        let mut read_half = read_half;
        while let Ok(Some(bytes)) = read_frame(&mut read_half).await {
            let Ok(response) = serde_json::from_slice::<RpcResponse>(&bytes) else {
                continue;
            };
            if tx.send(response).is_err() {
                break;
            }
        }
        // Loop exits on a closed/errored connection or a dropped receiver.
    });

    let mut state = ChatState::new(session_id);
    let mut next_id: u64 = 1;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    while !state.should_quit {
        while let Ok(response) = rx.try_recv() {
            match response.body {
                RpcResponseBody::Event(envelope) => state.fold_event(envelope),
                RpcResponseBody::Error { message } => state.push_log(format!("● error: {message}")),
                _ => {}
            }
        }

        terminal.draw(|frame| render::render(frame, &state))?;

        if event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = event::read()? {
                match input::map_key(key) {
                    InputAction::Insert(character) => state.input.push(character),
                    InputAction::Backspace => {
                        state.input.pop();
                    }
                    InputAction::Submit if !state.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut state.input);
                        state.push_log(format!("> {prompt}"));
                        send(
                            &mut write_half,
                            &mut next_id,
                            session_id,
                            RpcRequestBody::Prompt(UserInput {
                                text: prompt,
                                attachments: vec![],
                            }),
                        )
                        .await?;
                    }
                    InputAction::Cancel => {
                        send(&mut write_half, &mut next_id, session_id, RpcRequestBody::Cancel).await?;
                        state.push_log("● cancellation requested".to_string());
                    }
                    InputAction::Approve if state.pending_permission.is_some() => {
                        let id = state.pending_permission.take().expect("checked");
                        send(
                            &mut write_half,
                            &mut next_id,
                            session_id,
                            RpcRequestBody::ResolvePermission { id, decision: PermissionDecision::Approved },
                        )
                        .await?;
                        state.push_log("● permission approved".to_string());
                    }
                    InputAction::Reject if state.pending_permission.is_some() => {
                        let id = state.pending_permission.take().expect("checked");
                        send(
                            &mut write_half,
                            &mut next_id,
                            session_id,
                            RpcRequestBody::ResolvePermission { id, decision: PermissionDecision::Denied },
                        )
                        .await?;
                        state.push_log("● permission rejected".to_string());
                    }
                    InputAction::Quit => state.should_quit = true,
                    InputAction::Submit
                    | InputAction::Approve
                    | InputAction::Reject
                    | InputAction::Ignore => {}
                }
            }
        }
    }

    // Best-effort — the terminal is about to be restored either way.
    let _ = send(&mut write_half, &mut next_id, session_id, RpcRequestBody::CloseSession).await;
    Ok(())
}

async fn send(
    write_half: &mut OwnedWriteHalf,
    next_id: &mut u64,
    session_id: SessionId,
    body: RpcRequestBody,
) -> Result<()> {
    let request = RpcRequest {
        id: RequestCorrelationId(*next_id),
        session_id: Some(session_id),
        body,
    };
    *next_id += 1;
    write_frame(write_half, &serde_json::to_vec(&request)?).await?;
    Ok(())
}

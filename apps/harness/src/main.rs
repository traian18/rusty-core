#![warn(clippy::all)]

mod app_state;
mod harness_setup;
mod input;
mod render;

use anyhow::Result;
use app_state::AppState;
use clap::Parser;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use harness_protocol::commands::PermissionDecision;
use harness_setup::SessionOptions;
use input::InputAction;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};

#[derive(Parser, Debug)]
#[command(name = "harness")]
#[command(about = "Interactive TUI for testing the Rusty harness agent", long_about = None)]
struct Args {
    /// Integration backend to use (e.g., "anthropic", "claude-code")
    #[arg(long, default_value = "anthropic")]
    integration: String,

    /// Backend-specific configuration as JSON (e.g. '{"api_key": "...')
    #[arg(long, default_value = "{}")]
    config_json: String,
}

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let options = SessionOptions {
        integration: args.integration,
        config_json: args.config_json,
    };

    let session = harness_setup::start_session(std::env::current_dir()?, options).await?;
    let mut events = session.subscribe();
    let mut state = AppState::from_snapshot(session.snapshot().status);
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    while !state.should_quit {
        while let Ok(event) = events.try_recv() {
            state.fold_event(event);
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
                        state.events.push(format!("> {prompt}"));
                        session.send(&prompt).await?;
                    }
                    InputAction::Cancel => {
                        session.cancel().await?;
                        state.events.push("● cancellation requested".to_owned());
                    }
                    InputAction::Approve if state.pending_permission.is_some() => {
                        let id = state.pending_permission.take().expect("checked");
                        session
                            .resolve_permission(id, PermissionDecision::Approved)
                            .await?;
                        state.events.push("● permission approved".to_owned());
                    }
                    InputAction::Reject if state.pending_permission.is_some() => {
                        let id = state.pending_permission.take().expect("checked");
                        session
                            .resolve_permission(id, PermissionDecision::Denied)
                            .await?;
                        state.events.push("● permission rejected".to_owned());
                    }
                    InputAction::Quit => state.should_quit = true,
                    InputAction::Submit
                    | InputAction::Approve
                    | InputAction::Reject
                    | InputAction::Ignore => {}
                }
            }
        }
        state.status = format!("{:?}", session.snapshot().status);
    }
    Ok(())
}

#![warn(clippy::all)]

mod app_state;
mod controller;
mod harness_setup;
mod input;
mod markdown;
mod model;
mod providers;
mod render;

use anyhow::Result;
use app_state::ModalResult;
use clap::Parser;
use controller::AppController;
use crossterm::{
    cursor::Show,
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use harness_protocol::commands::PermissionDecision;
use harness_setup::{AppHarness, SessionOptions};
use input::InputAction;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, path::PathBuf, time::Duration};

#[derive(Parser, Debug)]
#[command(name = "harness")]
#[command(about = "Interactive TUI for testing the Rusty harness agent", long_about = None)]
struct Args {
    /// Integration backend to use (anthropic, claude-code, openai, codex, or github-copilot)
    #[arg(long, default_value = "anthropic")]
    integration: String,

    /// Backend-specific configuration as JSON
    #[arg(long, default_value = "{}")]
    config_json: String,

    /// MCP server(s) to connect, as `name=command[,arg1,arg2,...]` for a
    /// stdio server or `name=https://host/mcp` for an HTTP one. Repeatable.
    /// Discovered tools are registered as `mcp.<name>.<tool>` alongside the
    /// built-in toolset, for every session this TUI starts (including ones
    /// created later via the provider picker). Env vars, a working
    /// directory, request headers, or a non-default timeout aren't
    /// expressible through this flag — embed `harness-engine` directly and
    /// use `SessionBuilder::mcp_server` for those.
    #[arg(long = "mcp-server")]
    mcp_servers: Vec<String>,

    /// Extra directory to scan for `SKILL.md` files, on top of
    /// `<cwd>/.harness/skills` and `$HOME/.harness/skills`. Repeatable;
    /// later directories win on a name collision.
    #[arg(long = "skills-dir")]
    skills_dirs: Vec<PathBuf>,

    /// Disable filesystem skills entirely.
    ///
    /// Skills are on by default here, unlike over the daemon's RPC: this is
    /// a local, single-user TUI, the scanned directories belong to the
    /// person running it, and a session with no skill directories costs
    /// nothing — no prompt text and two unused tools.
    #[arg(long)]
    no_skills: bool,
}

/// Parses one `--mcp-server` value.
///
/// Two forms, told apart by whether the part after `=` looks like a URL:
///
/// - `name=command[,arg1,arg2,...]` — a stdio server
/// - `name=http://host/mcp` or `name=https://host/mcp` — an HTTP server
///
/// A URL is never a plausible executable name, so the discrimination is
/// unambiguous and needs no extra flag.
fn parse_mcp_server(raw: &str) -> Result<harness_engine::McpServerConfig> {
    let (name, rest) = raw.split_once('=').ok_or_else(|| {
        anyhow::anyhow!("--mcp-server value {raw:?} must be name=command[,arg,...] or name=URL")
    })?;
    if name.is_empty() {
        anyhow::bail!("--mcp-server value {raw:?} has an empty name before '='");
    }

    if rest.starts_with("http://") || rest.starts_with("https://") {
        return Ok(harness_engine::McpServerConfig::http(name, rest));
    }

    let mut parts = rest.split(',');
    let command = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("--mcp-server value {raw:?} is missing a command after '='")
        })?;
    Ok(harness_engine::McpServerConfig::new(name, command).args(parts))
}

struct TerminalGuard {
    alternate_screen: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let alternate_screen = supports_alternate_screen(std::env::var("TERM").ok().as_deref());
        if alternate_screen {
            execute!(io::stdout(), EnterAlternateScreen)?;
        }
        Ok(Self { alternate_screen })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.alternate_screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        } else {
            let _ = execute!(io::stdout(), Show);
            eprintln!();
        }
    }
}

fn supports_alternate_screen(term: Option<&str>) -> bool {
    !matches!(term, None | Some("") | Some("dumb"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mcp_servers = args
        .mcp_servers
        .iter()
        .map(|raw| parse_mcp_server(raw))
        .collect::<Result<Vec<_>>>()?;
    let options = SessionOptions {
        integration: args.integration,
        config_json: args.config_json,
    };
    let initial_selection = options.selection();
    let workspace_root = std::env::current_dir()?;

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.draw(|frame| {
        render::render_startup(
            frame,
            &initial_selection.provider,
            &initial_selection.model,
            &workspace_root,
        )
    })?;

    let skills = (!args.no_skills).then(|| {
        let mut config = harness_engine::SkillsConfig::new().workspace_root(workspace_root.clone());
        config.extra_roots = args.skills_dirs.clone();
        config
    });

    let app_harness = AppHarness::new(workspace_root, mcp_servers, skills).await?;
    let mut controller = AppController::new(app_harness, options, initial_selection).await?;
    let mut needs_draw = true;

    while !controller.state().should_quit {
        needs_draw |= controller.tick();
        if needs_draw {
            terminal.draw(|frame| render::render(frame, controller.state()))?;
            needs_draw = false;
        }

        if event::poll(Duration::from_millis(40))? {
            let terminal_event = event::read()?;
            needs_draw = true;
            if let Event::Key(key) = terminal_event {
                let action = input::map_key(
                    key,
                    controller.state().active_permission().is_some(),
                    controller.state().modal.is_some(),
                );
                match action {
                    InputAction::Insert(character) if controller.state().modal.is_some() => {
                        controller.state_mut().modal_insert(character);
                    }
                    InputAction::Insert(character) => controller.state_mut().input.push(character),
                    InputAction::Newline => controller.state_mut().input.push('\n'),
                    InputAction::Backspace if controller.state().modal.is_some() => {
                        controller.state_mut().modal_backspace();
                    }
                    InputAction::Backspace => {
                        controller.state_mut().input.pop();
                    }
                    InputAction::OpenCommands => controller.state_mut().open_commands(),
                    InputAction::ToggleInspector => {
                        controller.state_mut().toggle_context_inspector()
                    }
                    InputAction::ToggleLog => controller.state_mut().toggle_log(),
                    InputAction::NewSession => controller.state_mut().open_new_session(),
                    InputAction::PreviousSession => {
                        if let Err(error) = controller.previous_session().await {
                            controller.state_mut().set_start_error(error.to_string());
                        }
                    }
                    InputAction::NextSession => {
                        if let Err(error) = controller.next_session().await {
                            controller.state_mut().set_start_error(error.to_string());
                        }
                    }
                    InputAction::NavigateUp => controller.state_mut().modal_up(),
                    InputAction::NavigateDown => controller.state_mut().modal_down(),
                    InputAction::Confirm => {
                        let result = controller.state_mut().confirm_modal();
                        if let ModalResult::StartSession(selection) = result {
                            controller.state_mut().status = "Connecting".to_owned();
                            terminal.draw(|frame| render::render(frame, controller.state()))?;
                            if let Err(error) = controller.start_selected(selection).await {
                                controller.state_mut().open_new_session();
                                controller.state_mut().set_start_error(format!(
                                    "Could not start provider session: {error}"
                                ));
                            }
                        }
                    }
                    InputAction::Submit if !controller.state().input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut controller.state_mut().input);
                        match prompt.trim() {
                            "/exit" | "/quit" => controller.state_mut().should_quit = true,
                            "/context" => controller.state_mut().toggle_context_inspector(),
                            "/log" | "/logs" => controller.state_mut().toggle_log(),
                            "/login" | "/connect" => match controller.auth_instruction() {
                                Ok(instruction) => {
                                    controller.state_mut().system_notice(instruction)
                                }
                                Err(error) => {
                                    controller.state_mut().set_start_error(error.to_string())
                                }
                            },
                            "/models" => {
                                controller.state_mut().status = "Refreshing models".into();
                                match controller.refresh_active_models().await {
                                    Ok(()) => controller.state_mut().open_new_session(),
                                    Err(error) => controller.state_mut().set_start_error(format!(
                                        "Could not refresh models: {error}"
                                    )),
                                }
                            }
                            "/new" | "/providers" => {
                                controller.state_mut().open_new_session();
                            }
                            _ => match controller.send(&prompt).await {
                                Ok(()) => {
                                    controller.state_mut().submit_user_message(prompt);
                                    controller.sync_session_lists();
                                }
                                Err(error) => {
                                    controller
                                        .state_mut()
                                        .set_start_error(format!("Could not send prompt: {error}"));
                                }
                            },
                        }
                    }
                    InputAction::Cancel if controller.state().modal.is_some() => {
                        controller.state_mut().cancel_modal();
                    }
                    InputAction::Cancel => {
                        if let Err(error) = controller.cancel().await {
                            controller
                                .state_mut()
                                .set_start_error(format!("Could not cancel session: {error}"));
                        }
                    }
                    InputAction::Approve => {
                        if let Some(id) = controller.state().active_permission() {
                            controller
                                .resolve_permission(id, PermissionDecision::Approved)
                                .await?;
                            controller.state_mut().resolve_permission(id, true);
                        }
                    }
                    InputAction::Reject => {
                        if let Some(id) = controller.state().active_permission() {
                            controller
                                .resolve_permission(id, PermissionDecision::Denied)
                                .await?;
                            controller.state_mut().resolve_permission(id, false);
                        }
                    }
                    InputAction::ScrollUp => controller.state_mut().scroll_up(8),
                    InputAction::ScrollDown => controller.state_mut().scroll_down(8),
                    InputAction::Follow => controller.state_mut().follow_bottom(),
                    InputAction::Quit => controller.state_mut().should_quit = true,
                    InputAction::Submit | InputAction::Ignore => {}
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::supports_alternate_screen;

    #[test]
    fn dumb_or_missing_term_uses_the_main_screen() {
        assert!(!supports_alternate_screen(None));
        assert!(!supports_alternate_screen(Some("")));
        assert!(!supports_alternate_screen(Some("dumb")));
    }

    #[test]
    fn capable_terminal_uses_the_alternate_screen() {
        assert!(supports_alternate_screen(Some("xterm-256color")));
    }
}

use crate::app_state::AppState;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn render(frame: &mut Frame, state: &AppState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .split(frame.area());
    let permissions = if state.pending_permission.is_some() {
        "  •  y approve / n reject"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Harness TUI  •  {}  •  Esc/Ctrl-C cancel  •  q quit{permissions}",
            state.status
        ))
        .block(Block::default().borders(Borders::ALL).title(" Session ")),
        layout[0],
    );
    let events = if state.events.is_empty() {
        "Waiting for a prompt…".to_owned()
    } else {
        state.events.join("\n")
    };
    frame.render_widget(
        Paragraph::new(events)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Activity ")),
        layout[1],
    );
    frame.render_widget(
        Paragraph::new(format!("> {}", state.input))
            .block(Block::default().borders(Borders::ALL).title(" Prompt ")),
        layout[2],
    );
}

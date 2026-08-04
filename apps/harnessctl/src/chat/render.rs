use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::state::ChatState;

pub fn render(frame: &mut Frame, state: &ChatState) {
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
            "harnessctl chat  •  session {}  •  {}  •  Esc/Ctrl-C cancel  •  q quit{permissions}",
            state.session_id, state.status
        ))
        .block(Block::default().borders(Borders::ALL).title(" Session ")),
        layout[0],
    );

    let log = if state.log.is_empty() {
        "Waiting for a prompt…".to_owned()
    } else {
        state.log.join("\n")
    };
    frame.render_widget(
        Paragraph::new(log)
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

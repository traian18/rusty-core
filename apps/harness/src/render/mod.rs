use crate::app_state::AppState;
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Padding, Paragraph},
};
use std::path::Path;

mod chrome;
mod layout;
mod overlays;
mod sidebar;
#[cfg(test)]
mod tests;
mod theme;
mod transcript;

use chrome::*;
use layout::*;
use overlays::*;
use sidebar::*;
use theme::*;
use transcript::*;

pub fn render_startup(frame: &mut Frame, provider: &str, model: &str, workspace: &Path) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        frame.area(),
    );

    let area = centered_fixed(68, 12, frame.area());
    let workspace = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " RUSTY ",
            Style::default().fg(BG).bg(ACCENT).bold(),
        )),
        Line::from(Span::styled("Agent workspace", Style::default().fg(MUTED))),
        Line::from(""),
        Line::from(vec![
            Span::styled("Starting   ", Style::default().fg(MUTED)),
            Span::styled(provider.to_owned(), Style::default().fg(TEXT).bold()),
            Span::styled("  ·  ", Style::default().fg(SUBTLE)),
            Span::styled(model.to_owned(), Style::default().fg(ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("Workspace  ", Style::default().fg(MUTED)),
            Span::styled(workspace.to_owned(), Style::default().fg(TEXT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Loading providers and conversation history…",
            Style::default().fg(ACCENT).italic(),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(Style::default().fg(TEXT).bg(PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(ACCENT))
                    .padding(Padding::horizontal(2)),
            ),
        area,
    );
}

pub fn render(frame: &mut Frame, state: &AppState) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        frame.area(),
    );

    if let Some(modal) = &state.modal {
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());
        render_header(frame, layout[0], state);
        render_modal(frame, modal, state, layout[1]);
        render_status(frame, layout[2], state);
        return;
    }

    let body = if frame.area().width >= 100 {
        let columns =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(60)]).split(frame.area());
        render_sidebar(frame, columns[0], state);
        columns[1]
    } else {
        frame.area()
    };

    let composer_height = state.input.lines().count().clamp(1, 6) as u16 + 2;
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .split(body);

    render_header(frame, layout[0], state);
    render_transcript(frame, layout[1], state);
    render_composer(
        frame,
        layout[2],
        state,
        state.modal.is_none() && !state.context_inspector_open && !state.log_open,
    );
    render_status(frame, layout[3], state);

    if state.context_inspector_open {
        render_context_inspector(frame, state);
    }
    if state.log_open {
        render_log_panel(frame, state);
    }
}

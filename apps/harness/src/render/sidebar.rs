//! The session list on the left.

use crate::app_state::AppState;
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, List, ListItem, Padding},
};

use crate::render::theme::{ACCENT, BORDER, ERROR, MUTED, PANEL, SUBTLE, SUCCESS, TEXT};

pub(super) fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState) {
    let items = state
        .sessions
        .iter()
        .map(|session| {
            let active = Some(session.id) == state.active_session;
            let marker = if active { "●" } else { "○" };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{marker} "),
                        Style::default().fg(if active { SUCCESS } else { SUBTLE }),
                    ),
                    Span::styled(
                        session.title.clone(),
                        Style::default()
                            .fg(if active { TEXT } else { MUTED })
                            .bold(),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "   {} · {}{}",
                        session.provider,
                        session.model,
                        if session.restorable {
                            ""
                        } else {
                            " · history only"
                        }
                    ),
                    Style::default().fg(SUBTLE),
                )),
                Line::from(""),
            ])
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(items).style(Style::default().bg(PANEL)).block(
            Block::default()
                .title(" ◆ Sessions ")
                .title_style(Style::default().fg(ACCENT).bold())
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .padding(Padding::new(1, 1, 1, 0)),
        ),
        area,
    );
}

pub(super) fn status_color(status: &str) -> Color {
    let lowered = status.to_lowercase();
    if lowered.contains("error") || lowered.contains("fail") {
        ERROR
    } else if lowered.contains("run")
        || lowered.contains("active")
        || lowered.contains("process")
        || lowered.contains("connect")
    {
        ACCENT
    } else if lowered.contains("idle") || lowered.contains("ready") || lowered.contains("complet") {
        SUCCESS
    } else {
        MUTED
    }
}

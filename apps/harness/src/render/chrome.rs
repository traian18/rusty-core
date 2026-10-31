//! The header, the prompt composer and the status line around the transcript.

use crate::app_state::AppState;
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

use crate::render::sidebar::status_color;
use crate::render::theme::{ACCENT, BG, BORDER, ERROR, MUTED, PANEL, SUBTLE, TEXT, WARNING};
use unicode_width::UnicodeWidthStr;

pub(super) fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let dot_color = status_color(&state.status);
    let title = Line::from(vec![
        Span::styled(" RUSTY ", Style::default().fg(BG).bg(ACCENT).bold()),
        Span::raw("   "),
        Span::styled(&state.provider, Style::default().fg(TEXT).bold()),
        Span::styled("  ·  ", Style::default().fg(SUBTLE)),
        Span::styled(&state.model, Style::default().fg(ACCENT).bold()),
        Span::styled("   ", Style::default()),
        Span::styled("●", Style::default().fg(dot_color)),
        Span::styled(format!(" {}", state.status), Style::default().fg(MUTED)),
        Span::styled(
            "   Ctrl+N switch provider/model",
            Style::default().fg(SUBTLE).italic(),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().bg(PANEL))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(BORDER))
                    .padding(Padding::horizontal(1)),
            ),
        area,
    );
}

pub(super) fn render_composer(frame: &mut Frame, area: Rect, state: &AppState, focused: bool) {
    let waiting_permission = state.active_permission().is_some();
    let title = if waiting_permission {
        " Prompt · permission waiting "
    } else if focused {
        " Prompt "
    } else {
        " Prompt · not focused "
    };
    let content = if state.input.is_empty() {
        Text::from(Span::styled(
            "Ask anything…  (Ctrl+N to switch provider/model)",
            Style::default().fg(MUTED).italic(),
        ))
    } else {
        Text::from(state.input.clone())
    };
    let line_count = state.input.lines().count().max(1);
    let visible_rows = area.height.saturating_sub(2).max(1) as usize;
    let vertical_scroll = line_count.saturating_sub(visible_rows) as u16;

    let border_color = if waiting_permission {
        WARNING
    } else if focused {
        ACCENT
    } else {
        BORDER
    };

    frame.render_widget(
        Paragraph::new(content)
            .style(Style::default().fg(TEXT).bg(PANEL))
            .wrap(Wrap { trim: false })
            .scroll((vertical_scroll, 0))
            .block(
                Block::default()
                    .title(title)
                    .title_style(Style::default().fg(border_color).bold())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(border_color))
                    .padding(Padding::horizontal(1)),
            ),
        area,
    );

    if focused && area.width > 4 && area.height > 2 {
        let last_line = state.input.lines().last().unwrap_or("");
        let max_column = area.width.saturating_sub(4) as usize;
        let column = UnicodeWidthStr::width(last_line).min(max_column) as u16;
        let visible_line = line_count
            .saturating_sub(1)
            .saturating_sub(vertical_scroll as usize) as u16;
        frame.set_cursor_position(Position::new(
            area.x.saturating_add(2).saturating_add(column),
            area.y.saturating_add(1).saturating_add(visible_line),
        ));
    }
}

pub(super) fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let usage = state
        .usage
        .as_ref()
        .and_then(|usage| usage.metrics.total_tokens.value())
        .map(|tokens| format!("{tokens} tokens"))
        .unwrap_or_else(|| "— tokens".to_owned());
    let pending = state.pending_permissions.len();
    let permission = if pending == 0 {
        String::new()
    } else {
        format!(
            " · {pending} permission{}",
            if pending == 1 { "" } else { "s" }
        )
    };
    let help = match state.error_banner.as_deref() {
        Some(error) if state.modal.is_none() => format!(" ⚠ {error} "),
        _ => format!(
            " Ctrl+P commands · Ctrl+N switch provider/model · Ctrl+I context · Ctrl+↑/↓ sessions · Enter send · Ctrl+Q quit │ {usage}{permission} "
        ),
    };
    frame.render_widget(
        Paragraph::new(help)
            .style(
                Style::default()
                    .fg(if state.error_banner.is_some() && state.modal.is_none() {
                        ERROR
                    } else {
                        SUBTLE
                    })
                    .bg(PANEL),
            )
            .alignment(Alignment::Right),
        area,
    );
}

//! Modals and panels drawn over the screen: pickers, the context inspector and the activity log.

use crate::app_state::{AppState, ModalState};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::render::theme::{ACCENT, BG, ERROR, MUTED, PANEL, SUBTLE, SUCCESS, TEXT};

use crate::render::layout::{centered_rect, single_line_preview};

pub(super) fn render_modal(frame: &mut Frame, modal: &ModalState, state: &AppState, bounds: Rect) {
    let error = state.error_banner.as_deref();
    let horizontal_margin = if bounds.width >= 64 { 2 } else { 0 };
    let area = Rect::new(
        bounds.x.saturating_add(horizontal_margin),
        bounds.y,
        bounds
            .width
            .saturating_sub(horizontal_margin.saturating_mul(2)),
        bounds.height,
    );
    frame.render_widget(Clear, area);

    let (title, lines) = match modal {
        ModalState::Commands { selected } => command_lines(*selected),
        ModalState::Provider { selected } => provider_lines(state, *selected),
        ModalState::Account { provider } => account_lines(&state.providers[*provider]),
        ModalState::Model {
            provider,
            selected,
            value,
        } => model_lines(&state.providers[*provider], *selected, value),
    };

    let mut lines = lines;
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("⚠ {error}"),
            Style::default().fg(ERROR),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        match modal {
            ModalState::Model { .. } => "↑/↓ choose · type to edit · Enter confirm · Esc close",
            _ => "↑/↓ navigate · Enter confirm · Esc close",
        },
        Style::default().fg(SUBTLE),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT).bg(PANEL))
            .block(
                Block::default()
                    .title(title)
                    .title_style(Style::default().fg(BG).bg(ACCENT).bold())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(ACCENT))
                    .padding(Padding::new(2, 2, 1, 1)),
            ),
        area,
    );
}

type ModalLines = (&'static str, Vec<Line<'static>>);

fn command_lines(selected: usize) -> ModalLines {
    let commands = [
        "New session / switch provider · model",
        "Context inspector",
        "Activity log",
        "Quit",
    ];
    (
        " Command palette ",
        commands
            .iter()
            .enumerate()
            .map(|(index, command)| selectable_line(command, index == selected))
            .collect(),
    )
}

fn provider_lines(state: &AppState, selected: usize) -> ModalLines {
    (
        " Select provider ",
        state
            .providers
            .iter()
            .enumerate()
            .flat_map(|(index, provider)| {
                let status = if provider.ready { "●" } else { "○" };
                let status_color = if provider.ready { SUCCESS } else { ERROR };
                vec![
                    Line::from(vec![
                        Span::styled(
                            if index == selected { "› " } else { "  " },
                            Style::default().fg(ACCENT),
                        ),
                        Span::styled(format!("{status} "), Style::default().fg(status_color)),
                        Span::styled(
                            provider.name.clone(),
                            Style::default()
                                .fg(if index == selected { TEXT } else { MUTED })
                                .add_modifier(if index == selected {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                }),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!(
                            "      {} · {} models",
                            provider.account_hint,
                            provider.models.len()
                        ),
                        Style::default().fg(SUBTLE),
                    )),
                ]
            })
            .collect(),
    )
}

fn account_lines(provider: &crate::providers::ProviderOption) -> ModalLines {
    (
        " Select account ",
        vec![
            Line::from(Span::styled(
                provider.name.clone(),
                Style::default().fg(ACCENT).bold(),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Profile      ", Style::default().fg(MUTED)),
                Span::styled(provider.account_hint.clone(), Style::default().fg(TEXT)),
            ]),
            Line::from(vec![
                Span::styled("Connection   ", Style::default().fg(MUTED)),
                Span::styled(
                    provider.credential_state.clone(),
                    Style::default().fg(if provider.ready { SUCCESS } else { ERROR }),
                ),
            ]),
            Line::from(vec![
                Span::styled("Health       ", Style::default().fg(MUTED)),
                Span::styled(
                    provider.health_message.clone(),
                    Style::default().fg(if provider.ready { SUCCESS } else { ERROR }),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                if provider.ready {
                    "Authentication is owned by this profile or its official CLI."
                } else {
                    "Resolve the connection issue before starting this provider."
                },
                Style::default().fg(SUBTLE),
            )),
        ],
    )
}

fn model_lines(
    provider: &crate::providers::ProviderOption,
    selected: usize,
    value: &str,
) -> ModalLines {
    let mut lines = vec![
        Line::from(Span::styled(
            provider.name.clone(),
            Style::default().fg(ACCENT).bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            provider.model_hint.clone(),
            Style::default().fg(SUBTLE),
        )),
    ];
    let selected_index = if selected == usize::MAX { 0 } else { selected };
    let start = selected_index.saturating_sub(3);
    lines.extend(
        provider
            .models
            .iter()
            .enumerate()
            .skip(start)
            .take(7)
            .map(|(index, model)| selectable_line(model, index == selected)),
    );
    if provider.models.len() > 7 {
        lines.push(Line::from(Span::styled(
            format!("  {} models available", provider.models.len()),
            Style::default().fg(SUBTLE),
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled("Model ID", Style::default().fg(MUTED))),
        Line::from(Span::styled(
            format!("  {value}_"),
            Style::default().fg(TEXT).bg(Color::Rgb(30, 34, 43)),
        )),
    ]);
    (" Select model ", lines)
}

pub(super) fn render_context_inspector(frame: &mut Frame, state: &AppState) {
    let area = centered_rect(68, 56, frame.area());
    frame.render_widget(Clear, area);
    let lines = match &state.context {
        Some(context) => vec![
            Line::from(vec![Span::styled("Generation        ", Style::default().fg(MUTED)), Span::styled(context.generation.to_string(), Style::default().fg(TEXT))]),
            Line::from(vec![Span::styled("Estimated tokens  ", Style::default().fg(MUTED)), Span::styled(context.estimated_tokens.map(|value| value.to_string()).unwrap_or_else(|| "—".into()), Style::default().fg(ACCENT))]),
            Line::from(vec![Span::styled("Active checkpoint ", Style::default().fg(MUTED)), Span::styled(context.checkpoint.clone().unwrap_or_else(|| "—".into()), Style::default().fg(TEXT))]),
            Line::from(vec![Span::styled("Covered through   ", Style::default().fg(MUTED)), Span::styled(context.covered_through.clone().unwrap_or_else(|| "—".into()), Style::default().fg(TEXT))]),
            Line::from(vec![Span::styled("Pinned items      ", Style::default().fg(MUTED)), Span::styled(context.pinned_items.to_string(), Style::default().fg(TEXT))]),
            Line::from(vec![Span::styled("Last compacted    ", Style::default().fg(MUTED)), Span::styled(context.last_compacted_at.clone().unwrap_or_else(|| "—".into()), Style::default().fg(TEXT))]),
            Line::from(""),
            Line::from(Span::styled("Canonical transcript remains durable; this panel shows the bounded inference-view lineage.", Style::default().fg(SUBTLE))),
            Line::from(""),
            Line::from(Span::styled("Ctrl+I close", Style::default().fg(ACCENT))),
        ],
        None => vec![Line::from(Span::styled("Context state is not available for this session.", Style::default().fg(MUTED)))],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(TEXT).bg(PANEL))
            .block(
                Block::default()
                    .title(" Context inspector ")
                    .title_style(Style::default().fg(BG).bg(ACCENT).bold())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(ACCENT))
                    .padding(Padding::new(2, 2, 1, 1)),
            ),
        area,
    );
}

/// Raw, chronological event log — every `AgentEvent` the active session has
/// emitted (except the two highest-frequency streaming-delta kinds; see
/// `app_state::log_summary`), independent of the curated transcript. Where
/// the transcript shows a finished conversation, this shows the underlying
/// activity that produced it: state transitions, backend request timing,
/// tool-call lifecycles with raw arguments, permission requests, usage
/// updates — including everything that happens (or fails to happen)
/// between "Run started" and a terminal error, which the curated transcript
/// alone doesn't surface.
pub(super) fn render_log_panel(frame: &mut Frame, state: &AppState) {
    let area = centered_rect(84, 70, frame.area());
    frame.render_widget(Clear, area);

    // Show only the most recent entries that could plausibly fit the panel
    // height; this is a simple always-scrolled-to-bottom view rather than a
    // scrollable one, matching the context inspector's own lack of
    // scrolling for now.
    let visible_rows = area.height.saturating_sub(6) as usize;
    let start = state.log.len().saturating_sub(visible_rows.max(1));

    let mut lines: Vec<Line> = if state.log.is_empty() {
        vec![Line::from(Span::styled(
            "No activity recorded yet for this session.",
            Style::default().fg(MUTED),
        ))]
    } else {
        state.log[start..]
            .iter()
            .map(|entry| {
                let color = if entry.text.starts_with("FAILED") {
                    ERROR
                } else if entry.text.starts_with("state") || entry.text.starts_with("completed") {
                    ACCENT
                } else {
                    TEXT
                };
                Line::from(vec![
                    Span::styled(
                        format!("[{:>4}] ", entry.sequence),
                        Style::default().fg(SUBTLE),
                    ),
                    Span::styled(
                        single_line_preview(&entry.text, 200),
                        Style::default().fg(color),
                    ),
                ])
            })
            .collect()
    };
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Ctrl+L or /log close",
        Style::default().fg(ACCENT),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(TEXT).bg(PANEL))
            .block(
                Block::default()
                    .title(" Activity log ")
                    .title_style(Style::default().fg(BG).bg(ACCENT).bold())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(ACCENT))
                    .padding(Padding::new(2, 2, 1, 1)),
            ),
        area,
    );
}

pub(super) fn selectable_line(label: &str, selected: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if selected { "› " } else { "  " },
            Style::default().fg(ACCENT),
        ),
        Span::styled(
            label.to_owned(),
            Style::default()
                .fg(if selected { TEXT } else { MUTED })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ])
}

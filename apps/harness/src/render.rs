use crate::{
    app_state::{AppState, ModalState},
    markdown::{render_markdown, MarkdownTheme},
    model::{PermissionDisplayDecision, ToolCallState, TranscriptBlock},
};
use std::path::Path;
use unicode_width::UnicodeWidthStr;
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap},
};

const BG: Color = Color::Rgb(13, 15, 19);
const PANEL: Color = Color::Rgb(20, 23, 29);
const MUTED: Color = Color::Rgb(129, 138, 152);
const SUBTLE: Color = Color::Rgb(90, 98, 112);
const TEXT: Color = Color::Rgb(230, 234, 241);
const ACCENT: Color = Color::Rgb(122, 162, 255);
const SUCCESS: Color = Color::Rgb(115, 209, 158);
const WARNING: Color = Color::Rgb(240, 188, 96);
const ERROR: Color = Color::Rgb(240, 110, 120);
const BORDER: Color = Color::Rgb(48, 54, 66);

pub fn render_startup(frame: &mut Frame, provider: &str, model: &str, workspace: &Path) {
    frame.render_widget(Block::default().style(Style::default().bg(BG)), frame.area());

    let area = centered_fixed(68, 12, frame.area());
    let workspace = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(" RUSTY ", Style::default().fg(BG).bg(ACCENT).bold())),
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
    frame.render_widget(Block::default().style(Style::default().bg(BG)), frame.area());

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
        let columns = Layout::horizontal([Constraint::Length(30), Constraint::Min(60)])
            .split(frame.area());
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
        state.modal.is_none() && !state.context_inspector_open,
    );
    render_status(frame, layout[3], state);

    if state.context_inspector_open {
        render_context_inspector(frame, state);
    }
}

fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState) {
    let items = state
        .sessions
        .iter()
        .map(|session| {
            let active = Some(session.id) == state.active_session;
            let marker = if active { "●" } else { "○" };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(if active { SUCCESS } else { SUBTLE })),
                    Span::styled(
                        session.title.clone(),
                        Style::default().fg(if active { TEXT } else { MUTED }).bold(),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "   {} · {}{}",
                        session.provider,
                        session.model,
                        if session.restorable { "" } else { " · history only" }
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

fn status_color(status: &str) -> Color {
    let lowered = status.to_lowercase();
    if lowered.contains("error") || lowered.contains("fail") {
        ERROR
    } else if lowered.contains("run") || lowered.contains("active") || lowered.contains("process") || lowered.contains("connect") {
        ACCENT
    } else if lowered.contains("idle") || lowered.contains("ready") || lowered.contains("complet") {
        SUCCESS
    } else {
        MUTED
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
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
        Span::styled("   Ctrl+N switch provider/model", Style::default().fg(SUBTLE).italic()),
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

fn render_transcript(frame: &mut Frame, area: Rect, state: &AppState) {
    let lines = if state.transcript.is_empty() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Start with a prompt. Agent messages and tool activity will appear here.",
                Style::default().fg(MUTED),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Ctrl+N switch provider or model · Ctrl+P commands · Ctrl+I context",
                Style::default().fg(SUBTLE),
            )),
        ]
    } else {
        state.transcript.iter().flat_map(block_lines).collect()
    };

    let visible_height = area.height.saturating_sub(2) as usize;
    let bottom = lines.len().saturating_sub(visible_height) as u16;
    let scroll = bottom.saturating_sub(state.scroll);

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT).bg(BG))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(Block::default().padding(Padding::new(2, 2, 1, 1))),
        area,
    );
}

fn badge(label: &str, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::default().fg(fg).bg(bg).bold())
}

fn block_lines(block: &TranscriptBlock) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    match block {
        TranscriptBlock::UserMessage { text } => {
            lines.push(Line::from(badge("YOU", BG, ACCENT)));
            lines.extend(render_markdown(text, markdown_theme()));
        }
        TranscriptBlock::AssistantMessage { text, reasoning, complete, .. } => {
            let mut header = vec![badge("ASSISTANT", BG, SUCCESS)];
            if !*complete {
                header.push(Span::styled("  thinking…", Style::default().fg(MUTED).italic()));
            }
            lines.push(Line::from(header));
            if !reasoning.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  ‣ {}", single_line_preview(reasoning, 120)),
                    Style::default().fg(SUBTLE).italic(),
                )));
            }
            if text.is_empty() {
                lines.push(Line::from(Span::styled("  …", Style::default().fg(MUTED))));
            } else {
                lines.extend(render_markdown(text, markdown_theme()));
            }
        }
        TranscriptBlock::ToolCall { name, arguments, state, .. } => {
            let (label, color) = match state {
                ToolCallState::Requested => ("queued".to_owned(), MUTED),
                ToolCallState::Running => ("running".to_owned(), ACCENT),
                ToolCallState::Progress { status, fraction } => {
                    (format!("{status} · {:.0}%", fraction * 100.0), ACCENT)
                }
                ToolCallState::Succeeded { .. } => ("done".to_owned(), SUCCESS),
                ToolCallState::Failed { .. } => ("failed".to_owned(), ERROR),
            };
            lines.push(Line::from(vec![
                badge("TOOL", BG, WARNING),
                Span::styled(format!("  {name}"), Style::default().fg(TEXT).bold()),
                Span::styled(format!("  ({label})"), Style::default().fg(color)),
            ]));
            if !arguments.is_null() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", single_line_preview(&arguments.to_string(), 140)),
                    Style::default().fg(SUBTLE),
                )));
            }
            if let ToolCallState::Succeeded { preview } | ToolCallState::Failed { preview } = state {
                lines.push(Line::from(Span::styled(
                    format!("  {}", single_line_preview(preview, 160)),
                    Style::default().fg(MUTED),
                )));
            }
        }
        TranscriptBlock::Permission { tool_name, decision, .. } => {
            let (message, color) = match decision {
                None => (
                    format!("Permission needed for {tool_name} · Ctrl+Y allow / Ctrl+N deny"),
                    WARNING,
                ),
                Some(PermissionDisplayDecision::Approved) => (format!("Allowed {tool_name}"), SUCCESS),
                Some(PermissionDisplayDecision::Denied) => (format!("Denied {tool_name}"), ERROR),
            };
            lines.push(Line::from(vec![
                badge("PERMISSION", BG, color),
                Span::styled(format!("  {message}"), Style::default().fg(color).bold()),
            ]));
        }
        TranscriptBlock::ChildAgent { agent_id, outcome } => {
            let state = outcome.map(|value| format!("{value:?}")).unwrap_or_else(|| "running".to_owned());
            lines.push(Line::from(vec![
                badge("CHILD", BG, ACCENT),
                Span::styled(format!("  {}", short_id(&agent_id.to_string())), Style::default().fg(TEXT)),
                Span::styled(format!("  {state}"), Style::default().fg(MUTED)),
            ]));
        }
        TranscriptBlock::SystemNotice { text } => lines.push(Line::from(Span::styled(
            format!("  · {text}"),
            Style::default().fg(SUBTLE).italic(),
        ))),
        TranscriptBlock::Error { code, message } => lines.push(Line::from(vec![
            badge("ERROR", BG, ERROR),
            Span::styled(format!("  {code}  {message}"), Style::default().fg(TEXT)),
        ])),
    }
    lines
}

fn markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        text: TEXT,
        accent: ACCENT,
        muted: MUTED,
        code_background: PANEL,
    }
}

fn render_composer(frame: &mut Frame, area: Rect, state: &AppState, focused: bool) {
    let waiting_permission = state.active_permission().is_some();
    let title = if waiting_permission {
        " Prompt · permission waiting "
    } else if focused {
        " Prompt "
    } else {
        " Prompt · not focused "
    };
    let content = if state.input.is_empty() {
        Text::from(Span::styled("Ask anything…  (Ctrl+N to switch provider/model)", Style::default().fg(MUTED).italic()))
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

fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
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
        format!(" · {pending} permission{}", if pending == 1 { "" } else { "s" })
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

fn render_modal(frame: &mut Frame, modal: &ModalState, state: &AppState, bounds: Rect) {
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
        ModalState::Commands { selected } => {
            let commands = ["New session / switch provider · model", "Context inspector", "Quit"];
            (
                " Command palette ",
                commands
                    .iter()
                    .enumerate()
                    .map(|(index, command)| selectable_line(command, index == *selected))
                    .collect(),
            )
        }
        ModalState::Provider { selected } => (
            " Select provider ",
            state.providers
                .iter()
                .enumerate()
                .flat_map(|(index, provider)| {
                    let status = if provider.ready { "●" } else { "○" };
                    let status_color = if provider.ready { SUCCESS } else { ERROR };
                    vec![
                        Line::from(vec![
                            Span::styled(if index == *selected { "› " } else { "  " }, Style::default().fg(ACCENT)),
                            Span::styled(format!("{status} "), Style::default().fg(status_color)),
                            Span::styled(
                                provider.name.clone(),
                                Style::default()
                                    .fg(if index == *selected { TEXT } else { MUTED })
                                    .add_modifier(if index == *selected { Modifier::BOLD } else { Modifier::empty() }),
                            ),
                        ]),
                        Line::from(Span::styled(
                            format!("      {} · {} models", provider.account_hint, provider.models.len()),
                            Style::default().fg(SUBTLE),
                        )),
                    ]
                })
                .collect(),
        ),
        ModalState::Account { provider } => {
            let provider = &state.providers[*provider];
            (
                " Select account ",
                vec![
                    Line::from(Span::styled(provider.name.clone(), Style::default().fg(ACCENT).bold())),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Profile      ", Style::default().fg(MUTED)),
                        Span::styled(provider.account_hint.clone(), Style::default().fg(TEXT)),
                    ]),
                    Line::from(vec![
                        Span::styled("Connection   ", Style::default().fg(MUTED)),
                        Span::styled(provider.credential_state.clone(), Style::default().fg(
                            if provider.ready { SUCCESS } else { ERROR }
                        )),
                    ]),
                    Line::from(vec![
                        Span::styled("Health       ", Style::default().fg(MUTED)),
                        Span::styled(provider.health_message.clone(), Style::default().fg(if provider.ready { SUCCESS } else { ERROR })),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(
                        if provider.ready { "Authentication is owned by this profile or its official CLI." } else { "Resolve the connection issue before starting this provider." },
                        Style::default().fg(SUBTLE),
                    )),
                ],
            )
        }
        ModalState::Model {
            provider,
            selected,
            value,
        } => {
            let provider = &state.providers[*provider];
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
            let selected_index = if *selected == usize::MAX { 0 } else { *selected };
            let start = selected_index.saturating_sub(3);
            lines.extend(
                provider
                    .models
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(7)
                    .map(|(index, model)| selectable_line(model, index == *selected)),
            );
            if provider.models.len() > 7 {
                lines.push(Line::from(Span::styled(
                    format!("  {} models available", provider.models.len()),
                    Style::default().fg(SUBTLE),
                )));
            }
            lines.extend([
                Line::from(""),
                Line::from(Span::styled(
                    "Model ID",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    format!("  {value}_"),
                    Style::default().fg(TEXT).bg(Color::Rgb(30, 34, 43)),
                )),
            ]);
            (" Select model ", lines)
        }
    };

    let mut lines = lines;
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!("⚠ {error}"), Style::default().fg(ERROR))));
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


fn render_context_inspector(frame: &mut Frame, state: &AppState) {
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
        Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().fg(TEXT).bg(PANEL)).block(
            Block::default().title(" Context inspector ").title_style(Style::default().fg(BG).bg(ACCENT).bold())
                .borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(ACCENT)).padding(Padding::new(2, 2, 1, 1))
        ), area,
    );
}

fn selectable_line(label: &str, selected: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, Style::default().fg(ACCENT)),
        Span::styled(
            label.to_owned(),
            Style::default()
                .fg(if selected { TEXT } else { MUTED })
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ])
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn centered_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let available_width = area.width.saturating_sub(2).max(1);
    let available_height = area.height.saturating_sub(2).max(1);
    let width = width.min(available_width);
    let height = height.min(available_height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y.saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn single_line_preview(value: &str, limit: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= limit {
        flattened
    } else {
        format!("{}…", flattened.chars().take(limit).collect::<String>())
    }
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn startup_frame_is_visible_before_engine_initialization() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| {
                render_startup(
                    frame,
                    "Anthropic API",
                    "claude-sonnet",
                    Path::new("/tmp/rusty-core"),
                );
            })
            .expect("draw startup");

        let screen = screen_text(&terminal);
        assert!(screen.contains("RUSTY"));
        assert!(screen.contains("Loading providers and conversation history"));
        assert!(terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .all(|cell| cell.bg != Color::Reset));
    }

    #[test]
    fn provider_picker_fits_every_provider_in_an_80_by_24_terminal() {
        let selection = crate::providers::selection_for("anthropic", &serde_json::json!({}));
        let mut state = AppState::welcome(selection);
        state.open_new_session();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("draw provider picker");

        let screen = screen_text(&terminal);
        for provider in [
            "Anthropic API",
            "Claude Code",
            "OpenAI API",
            "OpenAI Codex",
            "GitHub Copilot",
        ] {
            assert!(screen.contains(provider), "missing provider: {provider}");
        }
        assert!(screen.contains("Enter confirm"));
    }
}

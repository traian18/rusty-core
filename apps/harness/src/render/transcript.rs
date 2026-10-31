//! The conversation: one block per message, reasoning, tool call or result.

use crate::{
    app_state::AppState,
    markdown::{render_markdown, MarkdownTheme},
    model::{PermissionDisplayDecision, ToolCallState, TranscriptBlock},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Padding, Paragraph, Wrap},
};

use crate::render::layout::{short_id, single_line_preview};
use crate::render::theme::{
    ACCENT, BG, ERROR, MAX_REASONING_CHARS, MAX_TOOL_OUTPUT_CHARS, MUTED, PANEL, SUBTLE, SUCCESS,
    TEXT, WARNING,
};

pub(super) fn render_transcript(frame: &mut Frame, area: Rect, state: &AppState) {
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

pub(super) fn badge(label: &str, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::default().fg(fg).bg(bg).bold())
}

pub(super) fn block_lines(block: &TranscriptBlock) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    match block {
        TranscriptBlock::UserMessage { text } => {
            lines.push(Line::from(badge("YOU", BG, ACCENT)));
            lines.extend(render_markdown(text, markdown_theme()));
        }
        TranscriptBlock::AssistantMessage {
            text,
            reasoning,
            complete,
            ..
        } => {
            let mut header = vec![badge("ASSISTANT", BG, SUCCESS)];
            if !*complete {
                header.push(Span::styled(
                    "  thinking…",
                    Style::default().fg(MUTED).italic(),
                ));
            }
            lines.push(Line::from(header));
            if !reasoning.is_empty() {
                lines.extend(capped_multiline(
                    reasoning,
                    MAX_REASONING_CHARS,
                    "  ‣ ",
                    Style::default().fg(SUBTLE).italic(),
                ));
            }
            if text.is_empty() {
                lines.push(Line::from(Span::styled("  …", Style::default().fg(MUTED))));
            } else {
                lines.extend(render_markdown(text, markdown_theme()));
            }
        }
        TranscriptBlock::ToolCall {
            name,
            arguments,
            state,
            ..
        } => {
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
            if let ToolCallState::Succeeded { preview } | ToolCallState::Failed { preview } = state
            {
                lines.extend(tool_output_lines(preview));
            }
        }
        TranscriptBlock::Permission {
            tool_name,
            decision,
            ..
        } => {
            let (message, color) = match decision {
                None => (
                    format!("Permission needed for {tool_name} · Ctrl+Y allow / Ctrl+N deny"),
                    WARNING,
                ),
                Some(PermissionDisplayDecision::Approved) => {
                    (format!("Allowed {tool_name}"), SUCCESS)
                }
                Some(PermissionDisplayDecision::Denied) => (format!("Denied {tool_name}"), ERROR),
            };
            lines.push(Line::from(vec![
                badge("PERMISSION", BG, color),
                Span::styled(format!("  {message}"), Style::default().fg(color).bold()),
            ]));
        }
        TranscriptBlock::ChildAgent { agent_id, outcome } => {
            let state = outcome
                .map(|value| format!("{value:?}"))
                .unwrap_or_else(|| "running".to_owned());
            lines.push(Line::from(vec![
                badge("CHILD", BG, ACCENT),
                Span::styled(
                    format!("  {}", short_id(&agent_id.to_string())),
                    Style::default().fg(TEXT),
                ),
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

pub(super) fn markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        text: TEXT,
        accent: ACCENT,
        muted: MUTED,
        code_background: PANEL,
    }
}

/// Renders `text` as one styled line per `\n`-separated line, each prefixed
/// with `prefix`, capped at `char_cap` characters with a trailing
/// truncation marker if it's longer. Unlike `single_line_preview`, this
/// preserves real line breaks instead of flattening everything onto one
/// line — used for anything that can legitimately span many lines (a
/// reasoning stream, a tool's stdout/stderr) and shouldn't be squashed.
pub(super) fn capped_multiline(
    text: &str,
    char_cap: usize,
    prefix: &str,
    style: Style,
) -> Vec<Line<'static>> {
    let total_chars = text.chars().count();
    let truncated = total_chars > char_cap;
    let shown: String = if truncated {
        text.chars().take(char_cap).collect()
    } else {
        text.to_owned()
    };

    let mut lines: Vec<Line<'static>> = shown
        .lines()
        .map(|line| Line::from(Span::styled(format!("{prefix}{line}"), style)))
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prefix.to_owned(), style)));
    }
    if truncated {
        lines.push(Line::from(Span::styled(
            format!(
                "{prefix}… (truncated for display, {} more chars)",
                total_chars - char_cap
            ),
            style,
        )));
    }
    lines
}

/// Renders a tool result preview. `shell.exec`'s output is a JSON object
/// shaped `{"stdout", "stderr", "exit_code"}` (see
/// `crates/tools/shell/src/executor.rs`) — recognized here and shown as
/// real multi-line stdout/stderr text instead of one escaped JSON blob.
/// Any other tool's output falls back to pretty-printed JSON, or the raw
/// string as-is if it isn't JSON at all (e.g. a failed tool's error text).
pub(super) fn tool_output_lines(preview: &str) -> Vec<Line<'static>> {
    let style = Style::default().fg(MUTED);
    let indent = "    ";

    let Ok(value) = serde_json::from_str::<serde_json::Value>(preview) else {
        return capped_multiline(preview, MAX_TOOL_OUTPUT_CHARS, "  ", style);
    };

    if let Some(object) = value.as_object() {
        if object.contains_key("stdout") || object.contains_key("stderr") {
            let mut lines = Vec::new();
            if let Some(code) = object.get("exit_code") {
                lines.push(Line::from(Span::styled(
                    format!("  exit code {code}"),
                    style,
                )));
            }
            let stdout = object.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let stderr = object.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            if !stdout.is_empty() {
                lines.push(Line::from(Span::styled("  stdout:", style)));
                lines.extend(capped_multiline(
                    stdout,
                    MAX_TOOL_OUTPUT_CHARS,
                    indent,
                    style,
                ));
            }
            if !stderr.is_empty() {
                let warn = Style::default().fg(WARNING);
                lines.push(Line::from(Span::styled("  stderr:", warn)));
                lines.extend(capped_multiline(
                    stderr,
                    MAX_TOOL_OUTPUT_CHARS,
                    indent,
                    warn,
                ));
            }
            if stdout.is_empty() && stderr.is_empty() {
                lines.push(Line::from(Span::styled("  (no output)", style)));
            }
            return lines;
        }
    }

    let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| preview.to_owned());
    capped_multiline(&pretty, MAX_TOOL_OUTPUT_CHARS, "  ", style)
}

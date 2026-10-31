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
        "OpenAI API",
        "OpenAI Codex",
        "GitHub Copilot",
    ] {
        assert!(screen.contains(provider), "missing provider: {provider}");
    }
    assert!(screen.contains("Enter confirm"));
}

fn flatten(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn tool_output_lines_splits_shell_stdout_and_stderr_with_real_newlines() {
    let preview = serde_json::json!({
        "stdout": "line one\nline two",
        "stderr": "a warning",
        "exit_code": 0
    })
    .to_string();

    let rendered = flatten(&tool_output_lines(&preview));
    assert!(rendered.contains("exit code 0"));
    assert!(rendered.contains("line one"));
    assert!(rendered.contains("line two"));
    assert!(rendered.contains("a warning"));
}

#[test]
fn tool_output_lines_falls_back_to_raw_text_for_non_json_preview() {
    let rendered = flatten(&tool_output_lines("PermissionDenied"));
    assert!(rendered.contains("PermissionDenied"));
}

#[test]
fn capped_multiline_preserves_line_breaks_and_marks_truncation() {
    let lines = capped_multiline("a\nb\nc", 100, "> ", Style::default());
    assert_eq!(flatten(&lines), "> a\n> b\n> c");

    let long = "x".repeat(50);
    let truncated = capped_multiline(&long, 10, "> ", Style::default());
    let rendered = flatten(&truncated);
    assert!(rendered.contains("truncated for display, 40 more chars"));
}

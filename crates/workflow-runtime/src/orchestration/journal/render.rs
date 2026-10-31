//! Writing a step's actions as the text another step reads.

use rusty_workflow_core::StepActions;

/// One line per kind of action, for prompts.
pub(crate) fn render_actions(actions: &StepActions) -> String {
    let mut lines = Vec::new();
    if !actions.files_changed.is_empty() {
        lines.push(format!("- Changed: {}", actions.files_changed.join(", ")));
    }
    if !actions.commands.is_empty() {
        let runs: Vec<String> = actions
            .commands
            .iter()
            .map(|run| {
                format!(
                    "`{}` ({})",
                    clip(&run.command, 120),
                    if run.failed { "failed" } else { "passed" }
                )
            })
            .collect();
        lines.push(format!("- Ran: {}", runs.join(", ")));
    }
    if !actions.research.is_empty() {
        lines.push(format!(
            "- Asked for research: {}",
            actions.research.join(" | ")
        ));
    }
    if !actions.locations.is_empty() {
        lines.push(format!(
            "- Research found (read these lines rather than searching again): {}",
            actions.locations.join(" | ")
        ));
    }
    if !actions.files_read.is_empty() {
        lines.push(format!("- Read: {}", actions.files_read.join(", ")));
    }
    if actions.tool_errors > 0 {
        lines.push(format!("- Tool calls that failed: {}", actions.tool_errors));
    }
    lines.join("\n")
}

pub(super) fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n[… shortened …]")
}

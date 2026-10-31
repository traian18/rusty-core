//! The shape that keeps a task small enough for one builder session: a `Files:` line listing
//! every file it changes, a `Check:` line with the command that proves it, a few criteria, and
//! instructions that leave nothing to discover or decide. Checked on a task list before it is
//! saved; tasks already ticked are left alone.
use crate::model::{Severity, Task, Warning};

/// Most files one task may change, tests included.
pub const MAX_FILES: usize = 4;
/// Most criteria one task may satisfy.
pub const MAX_CRITERIA: usize = 4;

/// Phrases that hand the builder an investigation or a decision instead of a change.
const OPEN_WORK: [&str; 14] = [
    "trace",
    "investigate",
    "find out",
    "figure out",
    "look into",
    "explore",
    "determine whether",
    "determine if",
    "determine how",
    "decide whether",
    "decide how",
    "decide which",
    "if possible",
    "if it cannot",
];

/// What a task's `Files:` line says.
#[derive(Debug, PartialEq)]
pub enum Files {
    /// No `Files:` line.
    Missing,
    /// `Files: none`: the task only runs checks.
    None,
    Listed(Vec<String>),
}

/// The files the task's `Files:` line lists: backticked paths, or else its comma-separated items.
pub fn files(task: &Task) -> Files {
    let Some(value) = field(task, "files") else {
        return Files::Missing;
    };
    let quoted: Vec<String> = value
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    if !quoted.is_empty() {
        return Files::Listed(quoted);
    }
    let listed: Vec<String> = value
        .split(',')
        .map(|item| item.trim().trim_end_matches('.').trim())
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect();
    match listed.as_slice() {
        [] => Files::Missing,
        [only] if only.eq_ignore_ascii_case("none") => Files::None,
        _ => Files::Listed(listed),
    }
}

/// The command of the task's `Check:` line.
pub fn check_command(task: &Task) -> Option<&str> {
    field(task, "check").filter(|command| !command.is_empty())
}

/// The value of the instruction line `<name>: value`, name matched without case.
fn field<'a>(task: &'a Task, name: &str) -> Option<&'a str> {
    task.instructions.lines().find_map(|line| {
        let (key, value) = line
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .split_once(':')?;
        key.trim_matches('*')
            .trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim())
    })
}

/// The first phrase of [`OPEN_WORK`] the instructions use as words.
fn open_work(task: &Task) -> Option<&'static str> {
    let text = task.instructions.to_lowercase();
    OPEN_WORK.into_iter().find(|phrase| {
        text.match_indices(phrase).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + phrase.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
    })
}

/// Everything about an open task's shape that makes it too big or too vague for one session.
pub fn check(path: &str, task: &Task, out: &mut Vec<Warning>) {
    if task.done {
        return;
    }
    let mut warn = |message: String| {
        out.push(Warning::new(
            path,
            Some(task.line),
            Severity::Warning,
            message,
        ))
    };
    let only_checks = match files(task) {
        Files::Missing => {
            warn(format!(
                "{} has no `Files:` line listing every file it changes (at most {MAX_FILES})",
                task.id
            ));
            false
        }
        Files::None => true,
        Files::Listed(files) if files.len() > MAX_FILES => {
            warn(format!(
                "{} changes {} files; at most {MAX_FILES} fit one session: split it, first the narrow change of the shared code with its callers, then the behaviour on top",
                task.id,
                files.len()
            ));
            false
        }
        Files::Listed(_) => false,
    };
    if check_command(task).is_none() {
        warn(format!(
            "{} has no `Check:` line with the command that proves it is done",
            task.id
        ));
    }
    if !only_checks && task.criteria.len() > MAX_CRITERIA {
        warn(format!(
            "{} satisfies {} criteria; at most {MAX_CRITERIA} fit one session: split it by criterion",
            task.id,
            task.criteria.len()
        ));
    }
    if let Some(phrase) = open_work(task) {
        warn(format!(
            "{} leaves work to discover or decide (\"{phrase}\"): settle it in the plan and write the concrete change, or report it as an open question for the person",
            task.id
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(criteria: &[&str], instructions: &str) -> Task {
        Task {
            id: "T3".into(),
            title: "Bind route".into(),
            done: false,
            line: 7,
            requirements: vec!["R1".into()],
            criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
            after: Vec::new(),
            instructions: instructions.into(),
        }
    }

    fn messages(task: &Task) -> Vec<String> {
        let mut out = Vec::new();
        check("specs/changes/0001-a/tasks.md", task, &mut out);
        out.into_iter().map(|w| w.message).collect()
    }

    #[test]
    fn a_small_concrete_task_passes() {
        let small = task(
            &["C1", "C2"],
            "Add `request_context` to `ExecutionRequest`.\nFiles: `crates/p/src/backend.rs`, `crates/p/src/backend/tests.rs`\nCheck: `cargo test -p rusty-protocol`",
        );
        assert_eq!(messages(&small), Vec::<String>::new());
        assert_eq!(
            files(&small),
            Files::Listed(vec![
                "crates/p/src/backend.rs".into(),
                "crates/p/src/backend/tests.rs".into()
            ])
        );
    }

    #[test]
    fn missing_lines_too_many_files_and_criteria_are_reported() {
        let big = task(
            &["C1", "C2", "C3", "C4", "C5"],
            "Files: a.rs, b.rs, c.rs, d.rs, e.rs",
        );
        let found = messages(&big);
        assert!(found[0].contains("changes 5 files"), "{found:?}");
        assert!(found[1].contains("no `Check:` line"), "{found:?}");
        assert!(found[2].contains("satisfies 5 criteria"), "{found:?}");
        assert!(messages(&task(&["C1"], "Check: `npm test`"))[0].contains("no `Files:` line"));
    }

    #[test]
    fn open_work_is_reported_but_not_inside_other_words() {
        let vague = task(
            &["C1"],
            "Trace the actual request-route construction point.\nFiles: `a.rs`\nCheck: `cargo test`",
        );
        assert!(messages(&vague)[0].contains("(\"trace\")"));
        let concrete = task(
            &["C1"],
            "Keep the tracer and the explorer view as they are.\nFiles: `a.rs`\nCheck: `cargo test`",
        );
        assert_eq!(messages(&concrete), Vec::<String>::new());
    }

    #[test]
    fn a_check_only_task_may_cover_many_criteria_and_ticked_tasks_are_left_alone() {
        let whole = task(
            &["C1", "C2", "C3", "C4", "C5"],
            "Files: none\nCheck: `cargo test --workspace`",
        );
        assert_eq!(messages(&whole), Vec::<String>::new());
        let mut done = task(&["C1"], "");
        done.done = true;
        assert_eq!(messages(&done), Vec::<String>::new());
    }
}

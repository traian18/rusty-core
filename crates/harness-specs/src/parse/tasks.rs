//! A change's `tasks.md`: `- [x] **T1** Title. R1 (C1, C2) after T0` with indented instructions.
use super::ids_with_prefix;
use crate::model::{Task, TasksDoc};

pub fn parse(text: &str) -> TasksDoc {
    let mut tasks: Vec<Task> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if let Some((done, rest)) = header(line) {
            tasks.push(read_task(done, rest, index + 1, tasks.len() + 1));
        } else if let Some(task) = tasks.last_mut() {
            if line.starts_with(char::is_whitespace) && !line.trim().is_empty() {
                if !task.instructions.is_empty() {
                    task.instructions.push('\n');
                }
                task.instructions.push_str(line.trim());
            }
        }
    }
    TasksDoc { tasks }
}

/// `- [ ] rest` or `- [x] rest` at the start of a line.
fn header(line: &str) -> Option<(bool, &str)> {
    let rest = line.strip_prefix("- [")?;
    let mark = rest.chars().next()?;
    let rest = rest[mark.len_utf8()..].strip_prefix("] ")?;
    match mark {
        ' ' => Some((false, rest)),
        'x' | 'X' => Some((true, rest)),
        _ => None,
    }
}

fn read_task(done: bool, rest: &str, line: usize, position: usize) -> Task {
    let (id, tail) = match rest.strip_prefix("**").and_then(|r| r.split_once("**")) {
        Some((id, tail)) => (id.trim().to_owned(), tail),
        None => {
            let (word, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            (word.to_owned(), tail)
        }
    };
    let id = if ids_with_prefix(&id, 'T')
        .first()
        .is_some_and(|(_, found)| *found == id)
    {
        id
    } else {
        format!("T{position}")
    };
    let (head, after) = match tail.rfind(" after ") {
        Some(at) => (
            &tail[..at],
            ids_with_prefix(&tail[at + 7..], 'T')
                .into_iter()
                .map(|(_, id)| id)
                .collect(),
        ),
        None => (tail, Vec::new()),
    };
    let first_requirement = ids_with_prefix(head, 'R').first().map(|(at, _)| *at);
    let title_end = first_requirement.unwrap_or(head.len());
    let title = head[..title_end]
        .trim()
        .trim_end_matches(['.', ',', ':', '-', '–'])
        .trim()
        .to_owned();
    let refs = &head[title_end..];
    Task {
        id,
        title,
        done,
        line,
        requirements: ids_with_prefix(refs, 'R')
            .into_iter()
            .map(|(_, id)| id)
            .collect(),
        criteria: ids_with_prefix(refs, 'C')
            .into_iter()
            .map(|(_, id)| id)
            .collect(),
        after,
        instructions: String::new(),
    }
}

/// `text` with the task's box ticked or unticked; `None` when there is no such task. Everything
/// else, line endings included, stays as it was.
pub fn set_done(text: &str, id: &str, done: bool) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let target = !changed
            && header(line.trim_end_matches(['\n', '\r']))
                .is_some_and(|(_, rest)| read_task(false, rest, index + 1, 0).id == id);
        if target {
            out.push_str("- [");
            out.push(if done { 'x' } else { ' ' });
            out.push_str(&line[4..]);
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS: &str = "# Tasks: 0007 Photo albums\n\n- [x] **T1** Album model and storage. R1 (C1, C2)\n  Add `Album` to `src/albums/model.rs`.\n  Persist it with the library.\n- [ ] **T2** Create and rename albums from the sidebar. R1 (C1) after T1\n  Wire the sidebar.\n- [ ] Plain task without an id\n";

    #[test]
    fn reads_title_references_and_instructions() {
        let doc = parse(TASKS);
        assert_eq!(doc.tasks.len(), 3);
        let first = &doc.tasks[0];
        assert!(first.done);
        assert_eq!(
            (first.id.as_str(), first.title.as_str(), first.line),
            ("T1", "Album model and storage", 3)
        );
        assert_eq!(first.requirements, ["R1"]);
        assert_eq!(first.criteria, ["C1", "C2"]);
        assert_eq!(
            first.instructions,
            "Add `Album` to `src/albums/model.rs`.\nPersist it with the library."
        );
        let second = &doc.tasks[1];
        assert_eq!(
            (second.after.as_slice(), second.criteria.as_slice()),
            (&["T1".to_owned()][..], &["C1".to_owned()][..])
        );
        assert!(!second.done);
    }

    #[test]
    fn a_task_without_an_id_is_numbered_by_position() {
        let third = &parse(TASKS).tasks[2];
        assert_eq!(third.id, "T3");
        assert!(third.requirements.is_empty());
    }

    #[test]
    fn ticking_changes_one_box_and_nothing_else() {
        let ticked = set_done(TASKS, "T2", true).unwrap();
        assert_eq!(ticked.replace("- [x] **T2**", "- [ ] **T2**"), TASKS);
        assert_eq!(
            set_done(&ticked, "T1", false)
                .unwrap()
                .matches("- [ ]")
                .count(),
            2
        );
        assert_eq!(set_done(TASKS, "T9", true), None);
    }

    #[test]
    fn ticking_keeps_windows_line_endings() {
        let text = "- [ ] **T1** One. R1\r\n- [ ] **T2** Two. R1\r\n";
        assert_eq!(
            set_done(text, "T2", true).unwrap(),
            "- [ ] **T1** One. R1\r\n- [x] **T2** Two. R1\r\n"
        );
    }
}

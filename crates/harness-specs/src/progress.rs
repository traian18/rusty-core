//! What an Implement run leaves in the change folder: ticked tasks, a document per finished task,
//! `verification.md`, and the status. Written from whatever the run got to, so a stopped or failed
//! run keeps its progress and the next run starts after it.
use serde::{Deserialize, Serialize};

use crate::{
    layout::{slugify, Layout},
    model::{Status, Warning},
    parse::{self, front},
    store::{SpecError, SpecStore},
    validate,
};

#[derive(Clone, Debug, Deserialize)]
pub struct TaskResult {
    pub id: String,
    /// The builder's report, as Markdown.
    pub built: String,
    /// The reviewer's verdict, as Markdown.
    pub review: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CriterionResult {
    pub id: String,
    /// `pass`, `fail` or `manual`.
    pub status: String,
    pub evidence: String,
    #[serde(default)]
    pub how_to_test: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Verification {
    pub summary: String,
    pub criteria: Vec<CriterionResult>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProgressInput {
    /// Tasks the queue accepted.
    pub tasks: Vec<TaskResult>,
    pub verification: Option<Verification>,
    /// The run finished: verification and the manual checks were accepted.
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProgressRecorded {
    pub ticked: Vec<String>,
    pub status: Status,
    pub warnings: Vec<Warning>,
}

pub fn record_progress(
    store: &dyn SpecStore,
    dir: &str,
    input: &ProgressInput,
    date: &str,
) -> Result<ProgressRecorded, SpecError> {
    let layout = Layout::of(store);
    let base = layout.change(dir);
    let tasks_path = format!("{base}/tasks.md");
    let mut tasks_text = store
        .read(&tasks_path)
        .ok_or_else(|| SpecError::new("tasks.md is missing"))?;
    let known = parse::tasks::parse(&tasks_text).tasks;
    let mut ticked = Vec::new();
    for result in &input.tasks {
        let Some(task) = known.iter().find(|t| t.id == result.id) else {
            continue;
        };
        if !task.done {
            if let Some(updated) = parse::tasks::set_done(&tasks_text, &task.id, true) {
                tasks_text = updated;
                ticked.push(task.id.clone());
            }
        }
        store.write(
            &task_document_path(store, &base, &task.id, &task.title),
            &task_document(&task.id, &task.title, result),
        )?;
    }
    if !ticked.is_empty() {
        store.write(&tasks_path, &tasks_text)?;
    }
    let title = change_title(store, &base, dir);
    if let Some(verification) = &input.verification {
        store.write(
            &format!("{base}/verification.md"),
            &verification_document(&title, verification, input.passed),
        )?;
    }
    let status = move_status(store, &base, dir, &tasks_text, input, &ticked, date)?;
    Ok(ProgressRecorded {
        ticked,
        status,
        warnings: validate::validate_change(store, &layout, dir, status),
    })
}

/// The existing document of the task (`T2-anything.md`), else `tasks/T2-<title slug>.md`.
fn task_document_path(store: &dyn SpecStore, base: &str, id: &str, title: &str) -> String {
    let prefix = format!("{}-", id.to_ascii_lowercase());
    let existing = store
        .files(&format!("{base}/tasks"))
        .into_iter()
        .find(|f| f.to_ascii_lowercase().starts_with(&prefix));
    format!(
        "{base}/tasks/{}",
        existing.unwrap_or_else(|| format!("{id}-{}.md", slugify(title)))
    )
}

fn task_document(id: &str, title: &str, result: &TaskResult) -> String {
    format!(
        "# {id}: {title}\n\n## Built\n\n{}\n\n## Review\n\n{}\n",
        result.built.trim(),
        result.review.trim()
    )
}

fn change_title(store: &dyn SpecStore, base: &str, dir: &str) -> String {
    store
        .read(&format!("{base}/change.md"))
        .and_then(|t| front::change(&t).title)
        .unwrap_or_else(|| dir.to_owned())
}

fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn verification_document(title: &str, verification: &Verification, passed: bool) -> String {
    let mut out = format!(
        "# Verification: {title}\n\nResult: {}\n\n{}\n\n",
        if passed { "passed" } else { "not passed" },
        verification.summary.trim()
    );
    out.push_str("| Criterion | Status | Evidence |\n|---|---|---|\n");
    for c in &verification.criteria {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            cell(&c.id),
            cell(&c.status),
            cell(&c.evidence)
        ));
    }
    let manual: Vec<&CriterionResult> = verification
        .criteria
        .iter()
        .filter(|c| c.status == "manual")
        .collect();
    if !manual.is_empty() {
        out.push_str("\n## Checks for a person\n\n");
        for c in manual {
            out.push_str(&format!("- {}: {}\n", c.id, c.how_to_test.trim()));
        }
    }
    out
}

/// building while tasks remain, verifying once all are ticked, done when the run passed. Never back.
fn move_status(
    store: &dyn SpecStore,
    base: &str,
    dir: &str,
    tasks_text: &str,
    input: &ProgressInput,
    ticked: &[String],
    date: &str,
) -> Result<Status, SpecError> {
    let path = format!("{base}/change.md");
    let mut text = store
        .read(&path)
        .unwrap_or_else(|| format!("---\nid: \"{dir}\"\nstatus: planned\n---\n# {dir}\n"));
    let current = front::change(&text).status.unwrap_or_default();
    let all = parse::tasks::parse(tasks_text).tasks;
    let target = if input.passed {
        Status::Done
    } else if !all.is_empty() && all.iter().all(|t| t.done) {
        Status::Verifying
    } else if all.iter().any(|t| t.done) {
        Status::Building
    } else {
        current
    };
    let status = current.max(target);
    if status != current {
        text = front::set_field(&text, "status", status.word());
    }
    if !ticked.is_empty() {
        text = front::add_history(
            &text,
            &format!("{date} Implement: ticked {}.", ticked.join(", ")),
        );
    }
    if input.verification.is_some() {
        text = front::add_history(
            &text,
            &format!(
                "{date} Verification {}.",
                if input.passed {
                    "passed"
                } else {
                    "did not pass"
                }
            ),
        );
    }
    store.write(&path, &text)?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn seeded() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-a/change.md", "---\nid: \"0001\"\ntitle: Albums\nstatus: planned\n---\n# Albums\n\n## History\n- 2026-10-22 Created.\n");
        store.insert(
            "specs/changes/0001-a/spec.md",
            "# A\n\n## Requirements\n### R1: One\n- C1: WHEN a THEN b\n- C2: WHEN c THEN d\n",
        );
        store.insert(
            "specs/changes/0001-a/tasks.md",
            "- [ ] **T1** Model. R1 (C1)\n- [ ] **T2** Sidebar. R1 (C2)\n",
        );
        store
    }
    fn result(id: &str) -> TaskResult {
        TaskResult {
            id: id.into(),
            built: "Changed src/a.rs.".into(),
            review: "C1 pass: src/a.rs:3".into(),
        }
    }
    fn input(
        tasks: Vec<TaskResult>,
        verification: Option<Verification>,
        passed: bool,
    ) -> ProgressInput {
        ProgressInput {
            tasks,
            verification,
            passed,
        }
    }

    #[test]
    fn finished_tasks_are_ticked_documented_and_the_status_follows() {
        let store = seeded();
        let done = record_progress(
            &store,
            "0001-a",
            &input(vec![result("T1")], None, false),
            "2026-10-24",
        )
        .unwrap();
        assert_eq!(
            (done.ticked.as_slice(), done.status),
            (&["T1".to_owned()][..], Status::Building)
        );
        assert!(store
            .read("specs/changes/0001-a/tasks.md")
            .unwrap()
            .starts_with("- [x] **T1**"));
        let doc = store
            .read("specs/changes/0001-a/tasks/T1-model.md")
            .unwrap();
        assert!(
            doc.starts_with("# T1: Model")
                && doc.contains("## Built\n\nChanged src/a.rs.")
                && doc.contains("## Review")
        );
        assert!(store
            .read("specs/changes/0001-a/change.md")
            .unwrap()
            .contains("- 2026-10-24 Implement: ticked T1."));
        let all = record_progress(
            &store,
            "0001-a",
            &input(vec![result("T1"), result("T2")], None, false),
            "2026-10-24",
        )
        .unwrap();
        assert_eq!(
            (all.ticked.as_slice(), all.status),
            (&["T2".to_owned()][..], Status::Verifying)
        );
    }

    #[test]
    fn a_document_that_exists_keeps_its_name() {
        let store = seeded();
        store.insert("specs/changes/0001-a/tasks/T1-my-name.md", "old");
        record_progress(
            &store,
            "0001-a",
            &input(vec![result("T1")], None, false),
            "d",
        )
        .unwrap();
        assert!(store
            .read("specs/changes/0001-a/tasks/T1-my-name.md")
            .unwrap()
            .contains("## Built"));
        assert!(store
            .read("specs/changes/0001-a/tasks/T1-model.md")
            .is_none());
    }

    #[test]
    fn a_passed_run_writes_the_verification_and_finishes_the_change() {
        let store = seeded();
        let verification = Verification {
            summary: "All good.".into(),
            criteria: vec![
                CriterionResult {
                    id: "C1".into(),
                    status: "pass".into(),
                    evidence: "src/a.rs:3 | covers it".into(),
                    how_to_test: String::new(),
                },
                CriterionResult {
                    id: "C2".into(),
                    status: "manual".into(),
                    evidence: "needs a device".into(),
                    how_to_test: "Open the sidebar.".into(),
                },
            ],
        };
        let done = record_progress(
            &store,
            "0001-a",
            &input(vec![result("T1"), result("T2")], Some(verification), true),
            "2026-10-24",
        )
        .unwrap();
        assert_eq!(done.status, Status::Done);
        let text = store.read("specs/changes/0001-a/verification.md").unwrap();
        assert!(text.contains("# Verification: Albums") && text.contains("Result: passed"));
        assert!(
            text.contains("| C1 | pass | src/a.rs:3 \\| covers it |")
                && text.contains("- C2: Open the sidebar.")
        );
        assert!(store
            .read("specs/changes/0001-a/change.md")
            .unwrap()
            .contains("Verification passed."));
    }

    #[test]
    fn tasks_the_file_does_not_know_are_ignored_and_the_status_never_goes_back() {
        let store = seeded();
        store.insert(
            "specs/changes/0001-a/change.md",
            "---\nstatus: verifying\n---\n# A\n",
        );
        let done = record_progress(
            &store,
            "0001-a",
            &input(vec![result("final-check")], None, false),
            "d",
        )
        .unwrap();
        assert!(done.ticked.is_empty());
        assert_eq!(done.status, Status::Verifying);
        assert!(store.files("specs/changes/0001-a/tasks").is_empty());
    }

    #[test]
    fn a_change_without_tasks_is_an_error() {
        let store = MemoryStore::default();
        assert!(record_progress(&store, "0001-a", &input(vec![], None, false), "d").is_err());
    }
}

//! Writing a workflow step's Markdown into the change folder. The model only answers; the host
//! decides which file it goes to, checks it, writes it and moves the status on.
use serde::{Deserialize, Serialize};

use crate::{
    layout::Layout,
    model::{ChangeFront, Status, Warning},
    parse::{self, front},
    store::{SpecError, SpecStore},
    validate,
};

/// The workflows that write into a change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Specify,
    Plan,
    /// Writes no documents through `record`; `progress::record_progress` handles its output.
    Implement,
    /// Surveys an existing project: writes the project file and the capability list (`record_start`).
    Start,
    /// Writes one capability's spec from the survey's list (`record_capability`).
    Capability,
}

impl Step {
    /// The files this step may write.
    pub fn documents(self) -> &'static [&'static str] {
        match self {
            // research.md: what Analyze found in the project and the code; Plan may revise it.
            Self::Specify => &["research.md", "spec.md", "checklist.md"],
            Self::Plan => &["research.md", "design.md", "tasks.md"],
            Self::Implement => &[],
            Self::Start => &["project.md", "onboarding.md"],
            Self::Capability => &["spec.md"],
        }
    }
    fn history(self) -> &'static str {
        match self {
            Self::Specify => "Spec written by Specify.",
            Self::Plan => "Design and tasks written by Plan.",
            Self::Implement => "Implemented.",
            Self::Start => "Project surveyed by Start specs.",
            Self::Capability => "Capability written.",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct DocumentInput {
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Recorded {
    pub written: Vec<String>,
    pub status: Status,
    pub warnings: Vec<Warning>,
}

/// Writes `documents` into change `dir` and updates `change.md`. Refuses names the step does not
/// own, empty replies, and changes that do not exist.
pub fn record(
    store: &dyn SpecStore,
    dir: &str,
    step: Step,
    documents: &[DocumentInput],
    date: &str,
) -> Result<Recorded, SpecError> {
    match step {
        // For these `dir` is not a change: nothing (Start) or a capability's name.
        Step::Start => return record_start(store, documents),
        Step::Capability => return record_capability(store, dir, documents),
        _ => {}
    }
    let layout = Layout::of(store);
    if !store.dirs(&layout.changes()).iter().any(|d| d == dir) {
        return Err(SpecError::new(format!("{dir} is not a change")));
    }
    let base = layout.change(dir);
    let mut written = Vec::new();
    for document in documents {
        if !step.documents().contains(&document.name.as_str()) {
            return Err(SpecError::new(format!(
                "{} cannot be written by this step",
                document.name
            )));
        }
        let text = clean(&document.text);
        if text.trim().is_empty() {
            return Err(SpecError::new(format!(
                "the step returned nothing for {}",
                document.name
            )));
        }
        let path = format!("{base}/{}", document.name);
        store.write(&path, &text)?;
        written.push(path);
    }
    let status = move_on(store, &base, dir, step, documents, date)?;
    Ok(Recorded {
        written,
        status,
        warnings: validate::validate_change(store, &layout, dir, status),
    })
}

/// Writes what the survey produced: `project.md` and the capability list `onboarding.md`.
/// A missing `README.md` is added. Existing files are replaced by the revised text.
pub fn record_start(
    store: &dyn SpecStore,
    documents: &[DocumentInput],
) -> Result<Recorded, SpecError> {
    let layout = Layout::of(store);
    crate::scaffold::init(store)?;
    let mut written = Vec::new();
    for document in documents {
        let path = match document.name.as_str() {
            "project.md" => layout.project(),
            "onboarding.md" => crate::onboarding::path(&layout),
            other => {
                return Err(SpecError::new(format!(
                    "{other} cannot be written by this step"
                )))
            }
        };
        let text = clean(&document.text);
        if text.trim().is_empty() {
            return Err(SpecError::new(format!(
                "the step returned nothing for {}",
                document.name
            )));
        }
        if document.name == "onboarding.md" && crate::onboarding::parse(&text).is_empty() {
            return Err(SpecError::new(
                "the survey listed no capability (## name sections with Purpose: and Paths:)",
            ));
        }
        store.write(&path, &text)?;
        written.push(path);
    }
    Ok(Recorded {
        written,
        status: Status::Draft,
        warnings: validate::validate_project(store, &layout),
    })
}

/// Writes one capability's spec, `capabilities/<name>/spec.md`. The name is the capability's folder
/// name; the spec is checked for files it names that do not exist.
pub fn record_capability(
    store: &dyn SpecStore,
    name: &str,
    documents: &[DocumentInput],
) -> Result<Recorded, SpecError> {
    let layout = Layout::of(store);
    if name.is_empty() || crate::layout::slugify(name) != name {
        return Err(SpecError::new(format!(
            "'{name}' is not a capability folder name"
        )));
    }
    let [document] = documents else {
        return Err(SpecError::new("a capability has exactly one spec.md"));
    };
    if document.name != "spec.md" {
        return Err(SpecError::new(format!(
            "{} cannot be written by this step",
            document.name
        )));
    }
    let text = clean(&document.text);
    if text.trim().is_empty() {
        return Err(SpecError::new("the step returned nothing for spec.md"));
    }
    let path = layout.capability_spec(name);
    store.write(&path, &text)?;
    Ok(Recorded {
        warnings: crate::drift::missing_paths(store, &path, &text),
        written: vec![path],
        status: Status::Draft,
    })
}

fn move_on(
    store: &dyn SpecStore,
    base: &str,
    dir: &str,
    step: Step,
    documents: &[DocumentInput],
    date: &str,
) -> Result<Status, SpecError> {
    let path = format!("{base}/change.md");
    let mut text = store.read(&path).unwrap_or_else(|| fresh_change(dir, date));
    let current = front::change(&text).status.unwrap_or_default();
    let target = match step {
        Step::Specify => Status::Specified,
        Step::Plan if documents.iter().any(|d| d.name == "tasks.md") => Status::Planned,
        Step::Plan | Step::Implement | Step::Start | Step::Capability => current,
    };
    let status = current.max(target);
    if status != current {
        text = front::set_field(&text, "status", status.word());
    }
    text = front::add_history(&text, &format!("{date} {}", step.history()));
    store.write(&path, &text)?;
    Ok(status)
}

/// A `change.md` for a folder that was written by hand.
fn fresh_change(dir: &str, date: &str) -> String {
    let (number, slug) = crate::layout::parse_dir_name(dir).unwrap_or((0, dir.to_owned()));
    let front = ChangeFront {
        id: Some(format!("{number:04}")),
        title: Some(slug.replace('-', " ")),
        status: Some(Status::Draft),
        created: Some(date.to_owned()),
        ..Default::default()
    };
    front::render_change(&front, &[])
}

/// What a model returned, made into a file: a whole-reply code fence is removed, and any preamble
/// before the first `# ` heading (within the first lines) is dropped. The text ends in one newline.
pub fn clean(reply: &str) -> String {
    let mut text = reply.trim().to_owned();
    if text.starts_with("```") && text.ends_with("```") && text.lines().count() >= 2 {
        let inner: Vec<&str> = text.lines().collect();
        text = inner[1..inner.len() - 1].join("\n").trim().to_owned();
    }
    let heading = text.lines().take(8).position(|line| line.starts_with("# "));
    if let Some(at) = heading.filter(|at| *at > 0) {
        text = text.lines().skip(at).collect::<Vec<_>>().join("\n");
    }
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

/// How many tasks `tasks.md` text holds; the page warns when a Plan reply has none.
pub fn task_count(text: &str) -> usize {
    parse::tasks::parse(text).tasks.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn seeded() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-albums/change.md", "---\nid: \"0001\"\ntitle: Albums\nstatus: draft\ncreated: 2026-10-22\ncapabilities: []\n---\n# Albums\n\n## History\n- 2026-10-22 Created.\n");
        store
    }
    fn input(name: &str, text: &str) -> DocumentInput {
        DocumentInput {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn specify_writes_the_spec_and_moves_a_draft_to_specified() {
        let store = seeded();
        let spec = "```markdown\nHere you go:\n# Albums\n\n## Requirements\n### R1: One\n- C1: WHEN a THEN b\n```";
        let done = record(
            &store,
            "0001-albums",
            Step::Specify,
            &[input("spec.md", spec)],
            "2026-10-23",
        )
        .unwrap();
        assert_eq!(
            (done.written.as_slice(), done.status),
            (
                &["specs/changes/0001-albums/spec.md".to_owned()][..],
                Status::Specified
            )
        );
        assert_eq!(
            store
                .read("specs/changes/0001-albums/spec.md")
                .unwrap()
                .lines()
                .next(),
            Some("# Albums")
        );
        let change = store.read("specs/changes/0001-albums/change.md").unwrap();
        assert!(
            change.contains("status: specified")
                && change.contains("- 2026-10-23 Spec written by Specify.")
        );
    }

    #[test]
    fn plan_moves_on_only_when_it_wrote_tasks_and_never_moves_a_change_back() {
        let store = seeded();
        let design = input("design.md", "# Design\n");
        assert_eq!(
            record(
                &store,
                "0001-albums",
                Step::Plan,
                std::slice::from_ref(&design),
                "2026-10-23"
            )
            .unwrap()
            .status,
            Status::Draft
        );
        let tasks = input("tasks.md", "# Tasks\n\n- [ ] **T1** One. R1 (C1)\n");
        assert_eq!(
            record(
                &store,
                "0001-albums",
                Step::Plan,
                &[design.clone(), tasks.clone()],
                "2026-10-23"
            )
            .unwrap()
            .status,
            Status::Planned
        );
        store.insert(
            "specs/changes/0001-albums/change.md",
            "---\nstatus: building\n---\n# A\n",
        );
        assert_eq!(
            record(&store, "0001-albums", Step::Plan, &[tasks], "2026-10-24")
                .unwrap()
                .status,
            Status::Building
        );
    }

    #[test]
    fn specify_also_writes_the_research_and_the_checklist() {
        let store = seeded();
        let done = record(
            &store,
            "0001-albums",
            Step::Specify,
            &[
                input("research.md", "# Research\n\nAlbums live in the store.\n"),
                input("spec.md", "# Albums\n\n## Requirements\n### R1: One\n- C1: WHEN a THEN b\n"),
                input("checklist.md", "# Checklist\n\n- [ ] CHK001 Is it clear?\n"),
            ],
            "2026-10-26",
        )
        .unwrap();
        assert_eq!(done.written.len(), 3);
        assert!(store.read("specs/changes/0001-albums/research.md").is_some());
        assert!(store.read("specs/changes/0001-albums/checklist.md").is_some());
    }

    #[test]
    fn a_step_cannot_write_files_it_does_not_own_or_nothing_at_all() {
        let store = seeded();
        assert!(record(
            &store,
            "0001-albums",
            Step::Specify,
            &[input("tasks.md", "# T\n")],
            "d"
        )
        .is_err());
        assert!(record(
            &store,
            "0001-albums",
            Step::Specify,
            &[input("spec.md", "  \n")],
            "d"
        )
        .is_err());
        assert!(record(
            &store,
            "9999-none",
            Step::Specify,
            &[input("spec.md", "# A\n")],
            "d"
        )
        .is_err());
    }

    #[test]
    fn a_folder_written_by_hand_gets_a_change_md() {
        let store = MemoryStore::default();
        store.insert("specs/changes/0004-by-hand/spec.md", "# By hand\n");
        record(
            &store,
            "0004-by-hand",
            Step::Specify,
            &[input("spec.md", "# By hand\n")],
            "2026-10-23",
        )
        .unwrap();
        let change =
            parse::front::change(&store.read("specs/changes/0004-by-hand/change.md").unwrap());
        assert_eq!(
            (change.id.as_deref(), change.status),
            (Some("0004"), Some(Status::Specified))
        );
    }

    #[test]
    fn warnings_come_back_with_the_result() {
        let store = seeded();
        let spec = input("spec.md", "# A\n\n## Requirements\n### R1: One\n");
        let done = record(&store, "0001-albums", Step::Specify, &[spec], "d").unwrap();
        assert!(done
            .warnings
            .iter()
            .any(|w| w.message == "R1 has no criteria"));
    }

    #[test]
    fn start_writes_the_project_and_the_capability_list() {
        let store = MemoryStore::default();
        let list = "# Onboarding\n\n## albums\nPurpose: grouping.\nPaths: `src/albums/`\n";
        let documents = [
            input("project.md", "# Project\n\n## Principles\n- small\n"),
            input("onboarding.md", list),
        ];
        let done = record_start(&store, &documents).unwrap();
        assert_eq!(done.written, ["specs/project.md", "specs/onboarding.md"]);
        assert!(store.read("specs/README.md").is_some());
        assert!(done.warnings.is_empty());
        assert!(record(
            &store,
            "",
            Step::Start,
            &[input("project.md", "# P\n")],
            "d"
        )
        .is_ok());
    }

    #[test]
    fn a_survey_without_capabilities_or_with_other_files_is_an_error() {
        let empty = MemoryStore::default();
        assert!(record_start(
            &empty,
            &[input("onboarding.md", "# Onboarding\n\nnothing\n")]
        )
        .is_err());
        assert!(record_start(&empty, &[input("spec.md", "# A\n")]).is_err());
        assert!(record_start(&empty, &[input("project.md", "  ")]).is_err());
        assert!(empty.read("specs/onboarding.md").is_none());
    }

    #[test]
    fn a_capability_spec_is_written_under_its_name_and_checked_for_drift() {
        let store = MemoryStore::default();
        store.insert("src/albums/model.rs", "x");
        let spec = "```markdown\n# Albums\n\nPurpose: grouping.\n\nKept in `src/albums/model.rs` and `src/albums/gone.rs`.\n```";
        let done = record(
            &store,
            "albums",
            Step::Capability,
            &[input("spec.md", spec)],
            "d",
        )
        .unwrap();
        assert_eq!(done.written, ["specs/capabilities/albums/spec.md"]);
        assert!(store
            .read("specs/capabilities/albums/spec.md")
            .unwrap()
            .starts_with("# Albums\n"));
        assert_eq!(done.warnings.len(), 1);
    }

    #[test]
    fn a_capability_needs_a_folder_name_one_spec_and_some_text() {
        let store = MemoryStore::default();
        let ok = input("spec.md", "# A\n");
        assert!(record_capability(&store, "Not A Name", std::slice::from_ref(&ok)).is_err());
        assert!(record_capability(&store, "", std::slice::from_ref(&ok)).is_err());
        assert!(record_capability(&store, "a", &[]).is_err());
        assert!(record_capability(&store, "a", &[input("design.md", "# A\n")]).is_err());
        assert!(record_capability(&store, "a", &[input("spec.md", " ")]).is_err());
    }

    #[test]
    fn clean_keeps_documents_that_are_already_clean() {
        assert_eq!(clean("# A\n\ntext"), "# A\n\ntext\n");
        assert_eq!(clean("   "), "");
        assert_eq!(task_count("- [ ] **T1** a. R1\n- [x] **T2** b. R1\n"), 2);
    }
}

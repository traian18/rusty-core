//! To GitHub Spec Kit: the project's principles as `.specify/memory/constitution.md`, and each
//! change as a feature folder `specs/NNN-slug/` with `spec.md`, `plan.md` (the design),
//! `research.md`, `tasks.md` and the quality checklist as `checklists/requirements.md`.
//!
//! A requirement becomes a user story whose criteria are its acceptance scenarios, and also a
//! functional requirement. Capability specs have no Spec Kit equivalent and are not exported.
use super::{note_export, scenario, source, write_new, ExportReport};
use crate::{
    import::common::{body, sections},
    layout::Layout,
    store::{SpecError, SpecStore},
};

const CONSTITUTION: &str = ".specify/memory/constitution.md";
const SPECS: &str = "specs";

pub fn export(store: &dyn SpecStore, date: &str) -> Result<ExportReport, SpecError> {
    let layout = Layout::of(store);
    let mut report = ExportReport::default();
    if let Some(project) = store.read(&layout.project()) {
        write_new(store, &mut report, CONSTITUTION, &constitution(&project))?;
    }
    for change in source::changes(store, &layout) {
        let feature = format!("{:03}-{}", change.number, change.slug);
        let base = format!("{SPECS}/{feature}");
        if store.read(&format!("{base}/spec.md")).is_some() {
            report.skipped.push(format!("{base} already exists"));
            continue;
        }
        write_new(
            store,
            &mut report,
            &format!("{base}/spec.md"),
            &spec(&change, &feature),
        )?;
        if let Some(checklist) = &change.checklist {
            write_new(
                store,
                &mut report,
                &format!("{base}/checklists/requirements.md"),
                &format!("{}\n", checklist.trim_end()),
            )?;
        }
        if let Some(design) = &change.design {
            write_new(
                store,
                &mut report,
                &format!("{base}/plan.md"),
                &format!("{}\n", design.trim_end()),
            )?;
        }
        if let Some(research) = &change.research {
            write_new(
                store,
                &mut report,
                &format!("{base}/research.md"),
                &format!("{}\n", research.trim_end()),
            )?;
        }
        if !change.tasks.is_empty() {
            write_new(
                store,
                &mut report,
                &format!("{base}/tasks.md"),
                &tasks(&change),
            )?;
        }
        note_export(store, &layout, &change, &base, date)?;
    }
    let capabilities = store.dirs(&layout.capabilities()).len();
    if capabilities > 0 {
        report.skipped.push(format!(
            "{capabilities} capability spec{} have no Spec Kit equivalent and were not exported",
            if capabilities == 1 { "" } else { "s" }
        ));
    }
    Ok(report)
}

/// The Principles of the project file, or the whole file when it has none.
fn constitution(project: &str) -> String {
    let found = sections(project)
        .into_iter()
        .find(|(h, _)| h == "principles")
        .map(|(_, lines)| body(&lines))
        .filter(|b| !b.is_empty());
    let principles = found.unwrap_or_else(|| {
        project
            .lines()
            .filter(|l| !l.starts_with("# "))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    });
    format!("# Project Constitution\n\n## Core Principles\n\n{principles}\n")
}

fn spec(change: &source::ChangeDoc, feature: &str) -> String {
    let input = change.why.lines().next().unwrap_or("").replace('"', "'");
    let mut out = format!(
        "# Feature Specification: {}\n\n**Feature Branch**: `{feature}`\n**Created**: {}\n**Status**: {}\n**Input**: User description: \"{input}\"\n\n## User Scenarios & Testing *(mandatory)*\n",
        change.title,
        change.created,
        if change.archived { "Complete" } else { "Draft" }
    );
    for (index, requirement) in change.requirements.iter().enumerate() {
        out.push_str(&format!(
            "\n### User Story {} - {} (Priority: P{})\n\n",
            index + 1,
            requirement.title,
            index + 1
        ));
        if !requirement.detail.is_empty() {
            out.push_str(&format!("{}\n\n", requirement.detail));
        }
        out.push_str("**Acceptance Scenarios**:\n\n");
        for (number, criterion) in requirement.criteria.iter().enumerate() {
            let s = scenario(&criterion.text);
            let then = s
                .and
                .iter()
                .fold(s.then.clone(), |acc, a| format!("{acc}, and {a}"));
            out.push_str(&format!(
                "{}. **Given** {}, **When** {}, **Then** {then}\n",
                number + 1,
                s.given.as_deref().unwrap_or("the feature is available"),
                s.when
            ));
        }
    }
    out.push_str("\n## Requirements *(mandatory)*\n\n### Functional Requirements\n\n");
    let mut number = 0;
    for requirement in &change.requirements {
        number += 1;
        let detail = if requirement.detail.is_empty() {
            String::new()
        } else {
            format!(" {}", requirement.detail.replace('\n', " "))
        };
        out.push_str(&format!(
            "- **FR-{number:03}**: {}.{detail}\n",
            requirement.title.trim_end_matches('.')
        ));
    }
    for question in &change.open_questions {
        number += 1;
        out.push_str(&format!(
            "- **FR-{number:03}**: [NEEDS CLARIFICATION: {question}]\n"
        ));
    }
    out
}

fn tasks(change: &source::ChangeDoc) -> String {
    let mut out = format!("# Tasks: {}\n\n## Phase 1: Tasks\n\n", change.title);
    for (index, task) in change.tasks.iter().enumerate() {
        out.push_str(&format!(
            "- [{}] T{:03} {}\n",
            if task.done { 'x' } else { ' ' },
            index + 1,
            task.title
        ));
        for line in task.instructions.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{import, model::Status, overview, store::MemoryStore};

    fn project() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert(
            "specs/project.md",
            "# Project\n\n## Stack\nRust\n\n## Principles\n- small\n- tested\n",
        );
        store.insert("specs/capabilities/albums/spec.md", "# Albums\n");
        store.insert("specs/changes/0007-rename/change.md", "---\nid: \"0007\"\ntitle: Rename albums\nstatus: building\ncreated: 2026-10-22\n---\n# Rename albums\n");
        store.insert("specs/changes/0007-rename/spec.md", "# Rename albums\n\n## Why\nPeople mistype names.\n\n## Requirements\n\n### R1: Renaming\nCapability: albums (adds \"Album renaming\")\nAlbums can be renamed.\n\n- C1: WHEN a name is given (given an album exists) THEN the album is renamed AND the old name is free.\n- C2: WHEN the name is taken THEN nothing changes.\n\n### R2: Naming rules\n- C3: WHEN x THEN y.\n\n## Open questions\n- Case sensitive?\n");
        store.insert("specs/changes/0007-rename/design.md", "# Design\nSmall.\n");
        store.insert(
            "specs/changes/0007-rename/research.md",
            "# Research\nNotes.\n",
        );
        store.insert(
            "specs/changes/0007-rename/tasks.md",
            "- [x] **T1** Model. R1 (C1)\n  Add it.\n- [ ] **T2** Sidebar. R1 (C2)\n",
        );
        store
    }

    #[test]
    fn the_checklist_becomes_the_requirements_checklist() {
        let store = project();
        store.insert(
            "specs/changes/0007-rename/checklist.md",
            "# Checklist\n\n- [ ] CHK001 Are name limits given? [Gap]\n",
        );
        export(&store, "2026-10-30").unwrap();
        assert_eq!(
            store
                .read("specs/007-rename/checklists/requirements.md")
                .unwrap(),
            "# Checklist\n\n- [ ] CHK001 Are name limits given? [Gap]\n"
        );
    }

    #[test]
    fn the_principles_become_the_constitution() {
        let store = project();
        export(&store, "2026-10-30").unwrap();
        assert_eq!(
            store.read(CONSTITUTION).unwrap(),
            "# Project Constitution\n\n## Core Principles\n\n- small\n- tested\n"
        );
    }

    #[test]
    fn a_change_becomes_a_feature_folder() {
        let store = project();
        let report = export(&store, "2026-10-30").unwrap();
        let spec = store.read("specs/007-rename/spec.md").unwrap();
        assert!(spec.starts_with("# Feature Specification: Rename albums\n\n**Feature Branch**: `007-rename`\n**Created**: 2026-10-22\n**Status**: Draft\n**Input**: User description: \"People mistype names.\""));
        assert!(spec.contains("### User Story 1 - Renaming (Priority: P1)\n\nAlbums can be renamed.\n\n**Acceptance Scenarios**:\n\n1. **Given** an album exists, **When** a name is given, **Then** the album is renamed, and the old name is free\n2. **Given** the feature is available, **When** the name is taken, **Then** nothing changes"));
        assert!(spec.contains("- **FR-001**: Renaming. Albums can be renamed.\n- **FR-002**: Naming rules.\n- **FR-003**: [NEEDS CLARIFICATION: Case sensitive?]"));
        assert!(store
            .read("specs/007-rename/plan.md")
            .unwrap()
            .contains("Small."));
        assert!(store
            .read("specs/007-rename/research.md")
            .unwrap()
            .contains("Notes."));
        assert_eq!(store.read("specs/007-rename/tasks.md").unwrap(), "# Tasks: Rename albums\n\n## Phase 1: Tasks\n\n- [x] T001 Model\n  Add it.\n- [ ] T002 Sidebar\n");
        assert!(report
            .skipped
            .iter()
            .any(|s| s.contains("1 capability spec have no Spec Kit equivalent")));
    }

    #[test]
    fn nothing_that_exists_is_overwritten() {
        let store = project();
        store.insert(CONSTITUTION, "mine");
        store.insert("specs/007-rename/spec.md", "mine");
        let report = export(&store, "d").unwrap();
        assert_eq!(store.read(CONSTITUTION).unwrap(), "mine");
        assert_eq!(store.read("specs/007-rename/spec.md").unwrap(), "mine");
        assert!(store.read("specs/007-rename/plan.md").is_none());
        assert_eq!(
            report
                .skipped
                .iter()
                .filter(|s| s.contains("already exists"))
                .count(),
            2
        );
    }

    #[test]
    fn what_is_exported_imports_back_with_its_stories_scenarios_tasks_and_ticks() {
        let store = project();
        export(&store, "d").unwrap();
        let back = MemoryStore::default();
        for path in store.paths().into_iter().filter(|p| {
            p.starts_with(".specify/")
                || p.starts_with("specs/0") && !p.starts_with("specs/changes")
        }) {
            back.insert(&path, &store.read(&path).unwrap());
        }
        let report = import::import(&back, import::Source::SpecKit, "2026-10-30").unwrap();
        assert!(report.project);
        assert_eq!(report.changes, ["0001-rename"]);
        let spec =
            crate::parse::spec::parse(&back.read("specs/changes/0001-rename/spec.md").unwrap());
        assert_eq!(spec.requirements[0].title, "Renaming");
        assert!(spec.requirements[0].criteria[0]
            .text
            .starts_with("WHEN a name is given (given an album exists) THEN the album is renamed"));
        assert_eq!(spec.open_questions.len(), 1);
        let found = overview(&back);
        assert_eq!(
            (
                found.changes[0].tasks_done,
                found.changes[0].tasks_total,
                found.changes[0].status
            ),
            (1, 2, Status::Building)
        );
    }

    #[test]
    fn importing_from_the_folder_just_written_does_not_bring_the_changes_back() {
        let store = project();
        let before = store.dirs("specs/changes").len();
        export(&store, "2026-10-30").unwrap();
        assert!(store
            .read("specs/changes/0007-rename/change.md")
            .unwrap()
            .contains("Exported to "));
        let report = import::import(&store, import::Source::SpecKit, "2026-10-31").unwrap();
        assert!(report.changes.is_empty(), "{report:?}");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("007-rename") && s.contains("imported before")),
            "{report:?}"
        );
        assert_eq!(store.dirs("specs/changes").len(), before);
    }
}

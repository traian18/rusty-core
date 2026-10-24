//! Everything under the specs root in one value: what the Specs page shows.
use crate::{
    drift,
    layout::{parse_dir_name, Layout, CHANGE_DOCUMENTS},
    model::{
        ArchivedChange, CapabilitySummary, ChangeSummary, Severity, SpecsOverview, Status, Warning,
    },
    parse,
    store::SpecStore,
    validate,
};

pub fn overview(store: &dyn SpecStore) -> SpecsOverview {
    let layout = Layout::of(store);
    let exists = !store.dirs(&layout.root).is_empty() || !store.files(&layout.root).is_empty();
    let mut warnings = Vec::new();
    let changes = changes(store, &layout, &mut warnings);
    SpecsOverview {
        root: layout.root.clone(),
        exists,
        project: store.read(&layout.project()).is_some(),
        project_warnings: if exists {
            project_warnings(store, &layout)
        } else {
            Vec::new()
        },
        capabilities: capabilities(store, &layout),
        onboarding: crate::onboarding::read(store, &layout),
        changes,
        archive: store
            .dirs(&layout.archive())
            .into_iter()
            .rev()
            .map(|dir| ArchivedChange {
                title: archived_title(store, &layout, &dir),
                dir,
            })
            .collect(),
        warnings,
    }
}

/// The project file's own checks, and the paths it names that are gone.
fn project_warnings(store: &dyn SpecStore, layout: &Layout) -> Vec<Warning> {
    let mut found = validate::validate_project(store, layout);
    if let Some(text) = store.read(&layout.project()) {
        found.extend(drift::missing_paths(store, &layout.project(), &text));
    }
    found
}

fn capabilities(store: &dyn SpecStore, layout: &Layout) -> Vec<CapabilitySummary> {
    store
        .dirs(&layout.capabilities())
        .into_iter()
        .map(|name| {
            let path = layout.capability_spec(&name);
            let text = store.read(&path);
            let doc = text
                .as_deref()
                .map(parse::capability::parse)
                .unwrap_or_default();
            let warnings = text
                .as_deref()
                .map(|text| drift::missing_paths(store, &path, text))
                .unwrap_or_default();
            CapabilitySummary {
                warnings,
                title: doc.title.unwrap_or_else(|| name.clone()),
                purpose: doc.purpose,
                requirements: doc.requirements.len(),
                path,
                name,
            }
        })
        .collect()
}

fn archived_title(store: &dyn SpecStore, layout: &Layout, dir: &str) -> String {
    let folder = format!("{}/{dir}", layout.archive());
    let front = store
        .read(&format!("{folder}/change.md"))
        .map(|t| parse::front::change(&t));
    front
        .and_then(|f| f.title)
        .unwrap_or_else(|| dir.to_owned())
}

fn changes(
    store: &dyn SpecStore,
    layout: &Layout,
    warnings: &mut Vec<Warning>,
) -> Vec<ChangeSummary> {
    let mut dirs = store.dirs(&layout.changes());
    dirs.sort();
    let mut numbers: Vec<(u32, String)> = Vec::new();
    let mut out = Vec::new();
    for dir in dirs {
        let number = parse_dir_name(&dir).map(|(n, _)| n);
        match number {
            Some(n) => {
                if let Some((_, other)) = numbers.iter().find(|(m, _)| *m == n) {
                    warnings.push(Warning::new(
                        &layout.change(&dir),
                        None,
                        Severity::Warning,
                        format!("{dir} has the same number as {other}"),
                    ));
                }
                numbers.push((n, dir.clone()));
            }
            None => warnings.push(Warning::new(
                &layout.change(&dir),
                None,
                Severity::Note,
                format!("{dir} is not named NNNN-slug"),
            )),
        }
        out.push(summarize(store, layout, &dir, number));
    }
    out
}

fn summarize(
    store: &dyn SpecStore,
    layout: &Layout,
    dir: &str,
    number: Option<u32>,
) -> ChangeSummary {
    let base = layout.change(dir);
    let documents: Vec<String> = CHANGE_DOCUMENTS
        .iter()
        .filter(|d| store.read(&format!("{base}/{d}")).is_some())
        .map(|d| (*d).to_owned())
        .collect();
    let has = |name: &str| documents.iter().any(|d| d == name);
    let front = store
        .read(&format!("{base}/change.md"))
        .map(|t| parse::front::change(&t));
    let spec = store
        .read(&format!("{base}/spec.md"))
        .map(|t| parse::spec::parse(&t));
    let tasks = store
        .read(&format!("{base}/tasks.md"))
        .map(|t| parse::tasks::parse(&t))
        .unwrap_or_default();
    let slug = parse_dir_name(dir).map_or_else(|| dir.to_owned(), |(_, slug)| slug);
    let status = front
        .as_ref()
        .and_then(|f| f.status)
        .unwrap_or_else(|| infer_status(has("spec.md"), has("tasks.md")));
    let (done, total) = (
        tasks.tasks.iter().filter(|t| t.done).count(),
        tasks.tasks.len(),
    );
    let mut warnings = validate::validate_change(store, layout, dir, status);
    if let Some(word) = front.as_ref().and_then(|f| f.unknown_status.as_ref()) {
        warnings.push(Warning::new(&format!("{base}/change.md"), None, Severity::Warning, format!("'{word}' is not a status; use draft, specified, planned, building, verifying, done or archived")));
    }
    ChangeSummary {
        dir: dir.to_owned(),
        number,
        title: front
            .as_ref()
            .and_then(|f| f.title.clone())
            .or_else(|| spec.as_ref().and_then(|s| s.title.clone()))
            .unwrap_or(slug),
        status,
        created: front.as_ref().and_then(|f| f.created.clone()),
        branch: front.as_ref().and_then(|f| f.branch.clone()),
        tasks_done: done,
        tasks_total: total,
        open_questions: spec.as_ref().map_or(0, |s| s.open_questions.len()),
        documents,
        task_documents: store.files(&format!("{base}/tasks")),
        next_step: status.next_step(),
        hint: hint(status, done, total),
        warnings,
    }
}

/// A folder written by hand has no `change.md`: the documents it holds say how far it got.
fn infer_status(has_spec: bool, has_tasks: bool) -> Status {
    match (has_spec, has_tasks) {
        (_, true) => Status::Planned,
        (true, false) => Status::Specified,
        (false, false) => Status::Draft,
    }
}

/// What the files say that the status does not; the page shows both and never rewrites the status.
fn hint(status: Status, done: usize, total: usize) -> Option<String> {
    if total == 0 {
        return None;
    }
    if status == Status::Done && done < total {
        return Some(format!(
            "The status is done but {} of {total} tasks are not ticked.",
            total - done
        ));
    }
    if done == total && status < Status::Verifying {
        return Some(format!(
            "Every task is ticked but the status is {}.",
            status.word()
        ));
    }
    if done > 0 && status < Status::Building {
        return Some(format!(
            "{done} of {total} tasks are ticked but the status is {}.",
            status.word()
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::NextStep, store::MemoryStore};

    fn seeded() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/project.md", "# P\n\n## Principles\n- small\n");
        store.insert("specs/capabilities/albums/spec.md", "# Albums\n\nPurpose: grouping.\n\n### Requirement: Creation\n### Requirement: Deletion\n");
        store.insert("specs/changes/0001-albums/change.md", "---\nid: \"0001\"\ntitle: Photo albums\nstatus: planned\nbranch: spec/0001-albums\n---\n# Photo albums\n");
        store.insert("specs/changes/0001-albums/spec.md", "# Photo albums\n\n## Requirements\n### R1: One\n- C1: WHEN a THEN b\n\n## Open questions\n- Nest?\n");
        store.insert(
            "specs/changes/0001-albums/tasks.md",
            "- [x] **T1** One. R1 (C1)\n",
        );
        store.insert("specs/changes/0001-albums/tasks/T1-one.md", "## Plan\n");
        store.insert("specs/changes/0002-by-hand/spec.md", "# By hand\n");
        store.insert(
            "specs/archive/2026-09-01-0000-first/change.md",
            "---\ntitle: First\n---\n",
        );
        store
    }

    #[test]
    fn capabilities_and_the_project_report_files_they_name_that_are_gone() {
        let store = seeded();
        store.insert("src/albums/model.rs", "x");
        store.insert(
            "specs/capabilities/albums/spec.md",
            "# Albums\n\nPurpose: grouping.\n\n### Requirement: Creation\nKept in `src/albums/model.rs` and `src/albums/gone.rs`.\n",
        );
        store.insert(
            "specs/project.md",
            "# P\n\n## Principles\n- small\n\nEntry point: `src/main/missing.rs`.\n",
        );
        let found = overview(&store);
        assert_eq!(found.capabilities[0].warnings.len(), 1);
        assert!(found.capabilities[0].warnings[0]
            .message
            .contains("src/albums/gone.rs"));
        assert!(found
            .project_warnings
            .iter()
            .any(|w| w.message.contains("src/main/missing.rs")));
    }

    #[test]
    fn nothing_under_the_root_means_it_does_not_exist() {
        let found = overview(&MemoryStore::default());
        assert!(!found.exists && found.changes.is_empty() && found.project_warnings.is_empty());
    }

    #[test]
    fn reports_capabilities_changes_progress_and_the_archive() {
        let found = overview(&seeded());
        assert!(found.exists && found.project && found.project_warnings.is_empty());
        assert_eq!(
            (
                found.capabilities[0].requirements,
                found.capabilities[0].title.as_str()
            ),
            (2, "Albums")
        );
        let first = &found.changes[0];
        assert_eq!(
            (first.title.as_str(), first.status, first.number),
            ("Photo albums", Status::Planned, Some(1))
        );
        assert_eq!(
            (first.tasks_done, first.tasks_total, first.open_questions),
            (1, 1, 1)
        );
        assert_eq!(first.documents, ["change.md", "spec.md", "tasks.md"]);
        assert_eq!(first.task_documents, ["T1-one.md"]);
        assert_eq!(first.branch.as_deref(), Some("spec/0001-albums"));
        assert_eq!(first.next_step, Some(NextStep::Implement));
        assert_eq!(found.archive[0].title, "First");
    }

    #[test]
    fn a_folder_without_change_md_gets_a_status_from_its_documents() {
        let found = overview(&seeded());
        let by_hand = &found.changes[1];
        assert_eq!(
            (by_hand.status, by_hand.title.as_str()),
            (Status::Specified, "By hand")
        );
        assert_eq!(by_hand.next_step, Some(NextStep::Plan));
    }

    #[test]
    fn ticks_that_disagree_with_the_status_produce_a_hint_not_a_new_status() {
        let found = overview(&seeded());
        let first = &found.changes[0];
        assert_eq!(first.status, Status::Planned);
        assert_eq!(
            first.hint.as_deref(),
            Some("Every task is ticked but the status is planned.")
        );
    }

    #[test]
    fn duplicate_numbers_and_odd_folder_names_are_reported() {
        let store = seeded();
        store.insert("specs/changes/0001-clash/spec.md", "# Clash\n");
        store.insert("specs/changes/notes/spec.md", "# Notes\n");
        let messages: Vec<String> = overview(&store)
            .warnings
            .into_iter()
            .map(|w| w.message)
            .collect();
        assert!(messages.contains(&"0001-clash has the same number as 0001-albums".to_owned()));
        assert!(messages.contains(&"notes is not named NNNN-slug".to_owned()));
    }
}

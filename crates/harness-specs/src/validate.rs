//! Deterministic checks for one change, shown as warnings next to its documents. They never block
//! reading; Plan, Implement and Archive run them first and show what they find.
use std::collections::{HashMap, HashSet};

use crate::{
    layout::Layout,
    model::{Severity, SpecDoc, Status, TasksDoc, Warning},
    parse,
    store::SpecStore,
};

/// Everything wrong or odd about the change in folder `dir`, most serious first.
pub fn validate_change(
    store: &dyn SpecStore,
    layout: &Layout,
    dir: &str,
    status: Status,
) -> Vec<Warning> {
    let base = layout.change(dir);
    let spec_path = format!("{base}/spec.md");
    let tasks_path = format!("{base}/tasks.md");
    let spec = store.read(&spec_path).map(|text| parse::spec::parse(&text));
    let tasks = store
        .read(&tasks_path)
        .map(|text| parse::tasks::parse(&text));
    let mut warnings = Vec::new();
    match &spec {
        Some(spec) => check_spec(&spec_path, spec, status, &mut warnings),
        None if status >= Status::Specified => {
            warnings.push(Warning::new(
                &spec_path,
                None,
                Severity::Warning,
                "spec.md is missing",
            ));
        }
        None => {}
    }
    match &tasks {
        Some(tasks) => check_tasks(&tasks_path, &spec_path, tasks, spec.as_ref(), &mut warnings),
        None if status >= Status::Planned => {
            warnings.push(Warning::new(
                &tasks_path,
                None,
                Severity::Warning,
                "tasks.md is missing",
            ));
        }
        None => {}
    }
    warnings.sort_by_key(|w| std::cmp::Reverse(w.severity));
    warnings
}

/// `project.md` should hold the principles the plan is checked against.
pub fn validate_project(store: &dyn SpecStore, layout: &Layout) -> Vec<Warning> {
    let path = layout.project();
    match store.read(&path) {
        None => vec![Warning::new(
            &path,
            None,
            Severity::Note,
            "project.md is missing",
        )],
        Some(text)
            if !text.lines().any(|l| {
                l.trim_start_matches('#')
                    .trim()
                    .eq_ignore_ascii_case("principles")
                    && l.starts_with('#')
            }) =>
        {
            vec![Warning::new(
                &path,
                None,
                Severity::Note,
                "project.md has no Principles section",
            )]
        }
        Some(_) => Vec::new(),
    }
}

fn check_spec(path: &str, spec: &SpecDoc, status: Status, out: &mut Vec<Warning>) {
    let mut seen = HashSet::new();
    for requirement in &spec.requirements {
        if requirement.criteria.is_empty() {
            out.push(Warning::new(
                path,
                Some(requirement.line),
                Severity::Warning,
                format!("{} has no criteria", requirement.id),
            ));
        }
        for criterion in &requirement.criteria {
            if !seen.insert(criterion.id.clone()) {
                out.push(Warning::new(
                    path,
                    Some(criterion.line),
                    Severity::Warning,
                    format!("{} is used more than once", criterion.id),
                ));
            }
            let upper = criterion.text.to_ascii_uppercase();
            if !(upper.contains("WHEN") && upper.contains("THEN")) {
                out.push(Warning::new(
                    path,
                    Some(criterion.line),
                    Severity::Note,
                    format!("{} is not in WHEN … THEN form", criterion.id),
                ));
            }
        }
    }
    if let Some((line, _)) = spec.open_questions.first() {
        let severity = if status >= Status::Planned {
            Severity::Warning
        } else {
            Severity::Note
        };
        let count = spec.open_questions.len();
        out.push(Warning::new(
            path,
            Some(*line),
            severity,
            format!("{count} open question{}", if count == 1 { "" } else { "s" }),
        ));
    }
}

fn check_tasks(
    path: &str,
    spec_path: &str,
    tasks: &TasksDoc,
    spec: Option<&SpecDoc>,
    out: &mut Vec<Warning>,
) {
    let mut ids = HashSet::new();
    for task in &tasks.tasks {
        if !ids.insert(task.id.as_str()) {
            out.push(Warning::new(
                path,
                Some(task.line),
                Severity::Warning,
                format!("{} is used more than once", task.id),
            ));
        }
    }
    for task in &tasks.tasks {
        for after in task.after.iter().filter(|a| !ids.contains(a.as_str())) {
            out.push(Warning::new(
                path,
                Some(task.line),
                Severity::Warning,
                format!("{} comes after {after}, which does not exist", task.id),
            ));
        }
    }
    if let Some(task) = find_cycle(tasks) {
        out.push(Warning::new(
            path,
            Some(task.1),
            Severity::Error,
            format!("{} depends on itself through its 'after' list", task.0),
        ));
    }
    let Some(spec) = spec else { return };
    let known_requirements: HashSet<&str> =
        spec.requirements.iter().map(|r| r.id.as_str()).collect();
    let known_criteria: HashSet<&str> = spec
        .requirements
        .iter()
        .flat_map(|r| &r.criteria)
        .map(|c| c.id.as_str())
        .collect();
    for task in &tasks.tasks {
        for id in task
            .requirements
            .iter()
            .filter(|id| !known_requirements.contains(id.as_str()))
        {
            out.push(Warning::new(
                path,
                Some(task.line),
                Severity::Warning,
                format!("{} names {id}, which is not in spec.md", task.id),
            ));
        }
        for id in task
            .criteria
            .iter()
            .filter(|id| !known_criteria.contains(id.as_str()))
        {
            out.push(Warning::new(
                path,
                Some(task.line),
                Severity::Warning,
                format!("{} names {id}, which is not in spec.md", task.id),
            ));
        }
    }
    let named: HashSet<&str> = tasks
        .tasks
        .iter()
        .flat_map(|t| &t.criteria)
        .map(String::as_str)
        .collect();
    for criterion in spec
        .requirements
        .iter()
        .flat_map(|r| &r.criteria)
        .filter(|c| !named.contains(c.id.as_str()))
    {
        out.push(Warning::new(
            spec_path,
            Some(criterion.line),
            Severity::Warning,
            format!("{} is not named by any task", criterion.id),
        ));
    }
}

/// A task on a cycle of `after` references: (id, line).
fn find_cycle(tasks: &TasksDoc) -> Option<(String, usize)> {
    let by_id: HashMap<&str, &crate::model::Task> =
        tasks.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a crate::model::Task>,
        marks: &mut HashMap<&'a str, Mark>,
    ) -> Option<&'a str> {
        match marks.get(id) {
            Some(Mark::Visiting) => return Some(id),
            Some(Mark::Done) => return None,
            None => {}
        }
        marks.insert(id, Mark::Visiting);
        if let Some(task) = by_id.get(id) {
            for next in task.after.iter().filter(|a| by_id.contains_key(a.as_str())) {
                if let Some(found) = visit(next, by_id, marks) {
                    return Some(found);
                }
            }
        }
        marks.insert(id, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    tasks
        .tasks
        .iter()
        .find_map(|task| visit(&task.id, &by_id, &mut marks))
        .map(|id| (id.to_owned(), by_id[id].line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn check(spec: &str, tasks: Option<&str>, status: Status) -> Vec<String> {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-a/spec.md", spec);
        if let Some(tasks) = tasks {
            store.insert("specs/changes/0001-a/tasks.md", tasks);
        }
        validate_change(&store, &Layout::new("specs"), "0001-a", status)
            .into_iter()
            .map(|w| w.message)
            .collect()
    }

    const SPEC: &str =
        "# A\n\n## Requirements\n\n### R1: One\n- C1: WHEN a THEN b\n- C2: WHEN c THEN d\n";

    #[test]
    fn a_consistent_change_has_no_warnings() {
        let tasks = "- [ ] **T1** One. R1 (C1, C2)\n";
        assert!(check(SPEC, Some(tasks), Status::Planned).is_empty());
    }

    #[test]
    fn criteria_no_task_names_are_reported_on_the_spec_line() {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-a/spec.md", SPEC);
        store.insert(
            "specs/changes/0001-a/tasks.md",
            "- [ ] **T1** One. R1 (C1)\n",
        );
        let found = validate_change(&store, &Layout::new("specs"), "0001-a", Status::Planned);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].message.as_str(), found[0].line),
            ("C2 is not named by any task", Some(7))
        );
        assert!(found[0].file.ends_with("spec.md"));
    }

    #[test]
    fn tasks_that_name_nothing_real_are_reported() {
        let found = check(
            SPEC,
            Some("- [ ] **T1** One. R9 (C1, C2, C7) after T4\n"),
            Status::Planned,
        );
        assert!(found.contains(&"T1 names R9, which is not in spec.md".to_owned()));
        assert!(found.contains(&"T1 names C7, which is not in spec.md".to_owned()));
        assert!(found.contains(&"T1 comes after T4, which does not exist".to_owned()));
    }

    #[test]
    fn a_cycle_is_an_error_and_sorted_first() {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-a/spec.md", SPEC);
        store.insert(
            "specs/changes/0001-a/tasks.md",
            "- [ ] **T1** One. R1 (C1) after T2\n- [ ] **T2** Two. R1 (C2) after T1\n",
        );
        let found = validate_change(&store, &Layout::new("specs"), "0001-a", Status::Planned);
        assert_eq!(found[0].severity, Severity::Error);
    }

    #[test]
    fn open_questions_matter_more_once_planned() {
        let spec = format!("{SPEC}\n## Open questions\n- Nest?\n");
        let severity = |status| {
            let store = MemoryStore::default();
            store.insert("specs/changes/0001-a/spec.md", &spec);
            validate_change(&store, &Layout::new("specs"), "0001-a", status)
                .into_iter()
                .find(|w| w.message.contains("open question"))
                .unwrap()
                .severity
        };
        assert_eq!(severity(Status::Specified), Severity::Note);
        assert_eq!(severity(Status::Planned), Severity::Warning);
    }

    #[test]
    fn requirements_without_criteria_and_loose_criteria_are_reported() {
        let found = check(
            "## Requirements\n### R1: One\n- C1: it works\n### R2: Two\n",
            None,
            Status::Draft,
        );
        assert!(found.contains(&"R2 has no criteria".to_owned()));
        assert!(found.contains(&"C1 is not in WHEN … THEN form".to_owned()));
    }

    #[test]
    fn missing_documents_are_reported_only_once_the_status_expects_them() {
        let store = MemoryStore::default();
        let layout = Layout::new("specs");
        assert!(validate_change(&store, &layout, "0001-a", Status::Draft).is_empty());
        let found = validate_change(&store, &layout, "0001-a", Status::Planned);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn the_project_needs_a_principles_section() {
        let store = MemoryStore::default();
        let layout = Layout::new("specs");
        assert_eq!(validate_project(&store, &layout).len(), 1);
        store.insert("specs/project.md", "# P\n\n## Stack\n");
        assert_eq!(
            validate_project(&store, &layout)[0].message,
            "project.md has no Principles section"
        );
        store.insert("specs/project.md", "# P\n\n## Principles\n- a\n");
        assert!(validate_project(&store, &layout).is_empty());
    }
}

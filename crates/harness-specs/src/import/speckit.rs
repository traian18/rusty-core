//! GitHub Spec Kit: `.specify/memory/constitution.md` and one folder per feature under `specs/`
//! (`NNN-name/` with `spec.md`, `plan.md`, `research.md`, `data-model.md`, `contracts/`,
//! `quickstart.md`, `tasks.md`).
//!
//! The spec is rebuilt in this layout (one requirement per user story, with its acceptance scenarios
//! as criteria, and one for the functional requirements), the plan becomes the design, and the task
//! list is renumbered. Files that have no equivalent here stay where they are and are listed in the
//! design.
use super::{common, Detected, ImportReport, Source};
use crate::{
    layout::Layout,
    store::{SpecError, SpecStore},
};

const CONSTITUTION: &str = ".specify/memory/constitution.md";
const SPECS: &str = "specs";
/// Files of a feature folder that this layout has no place for.
const LEFT_IN_PLACE: [&str; 2] = ["data-model.md", "quickstart.md"];

/// Feature folders are numbered (`001-photo-albums`); this layout's own folders are not.
fn feature_dirs(store: &dyn SpecStore) -> Vec<String> {
    store
        .dirs(SPECS)
        .into_iter()
        .filter(|name| {
            name.split_once('-').is_some_and(|(number, rest)| {
                !rest.is_empty() && number.len() >= 3 && number.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .filter(|name| store.read(&format!("{SPECS}/{name}/spec.md")).is_some())
        .collect()
}

pub fn detect(store: &dyn SpecStore) -> Option<Detected> {
    let changes = feature_dirs(store).len();
    let constitution = store.read(CONSTITUTION).is_some();
    if changes == 0 && !constitution {
        return None;
    }
    Some(Detected {
        source: Source::SpecKit,
        path: if changes > 0 {
            SPECS.into()
        } else {
            ".specify".into()
        },
        capabilities: 0,
        changes,
        archived: 0,
    })
}

pub fn import(store: &dyn SpecStore, date: &str) -> Result<ImportReport, SpecError> {
    let layout = Layout::of(store);
    let mut report = ImportReport::default();
    if let Some(text) = store.read(CONSTITUTION) {
        if store.read(&layout.project()).is_some() {
            report
                .skipped
                .push(format!("{} already exists", layout.project()));
        } else {
            let body: Vec<&str> = text.lines().filter(|l| !l.starts_with("# ")).collect();
            let project = format!(
                "# Project\n\nImported from the Spec Kit constitution.\n\n## Principles\n{}\n",
                body.join("\n").trim()
            );
            store.write(&layout.project(), &project)?;
            report.project = true;
        }
    }
    for name in feature_dirs(store) {
        let marker = format!("{SPECS}/{name}");
        if common::imported_before(store, &marker) {
            report
                .skipped
                .push(format!("feature {name} was imported before"));
            continue;
        }
        report.changes.push(feature(store, &name, &marker, date)?);
    }
    Ok(report)
}

fn feature(
    store: &dyn SpecStore,
    name: &str,
    marker: &str,
    date: &str,
) -> Result<String, SpecError> {
    let base = format!("{SPECS}/{name}");
    let spec_text = store.read(&format!("{base}/spec.md")).unwrap_or_default();
    let slug = name.split_once('-').map_or(name, |(_, rest)| rest);
    let title = spec_text
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| {
            t.trim()
                .trim_start_matches("Feature Specification:")
                .trim()
                .to_owned()
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| common::humanize(slug));
    let tasks = store
        .read(&format!("{base}/tasks.md"))
        .map(|t| convert_tasks(&t));
    let done: Vec<bool> = tasks
        .as_ref()
        .map(|(_, done)| done.clone())
        .unwrap_or_default();
    let mut files = vec![("spec.md", spec(&title, name, &spec_text))];
    let design = design(store, &base, name);
    if let Some(design) = design {
        files.push(("design.md", design));
    }
    if let Some(research) = store.read(&format!("{base}/research.md")) {
        files.push(("research.md", format!("{}\n", research.trim_end())));
    }
    if let Some((text, _)) = tasks {
        files.push(("tasks.md", text));
    }
    let status = common::status_from(true, &done);
    common::write_change(
        store,
        &common::Converted {
            title: &title,
            name: slug,
            status,
            date,
            marker,
            files,
        },
    )
}

/// `plan.md`, with a note of the Spec Kit files this layout has no place for.
fn design(store: &dyn SpecStore, base: &str, name: &str) -> Option<String> {
    let plan = store.read(&format!("{base}/plan.md"))?;
    let mut left: Vec<String> = LEFT_IN_PLACE
        .iter()
        .filter(|f| store.read(&format!("{base}/{f}")).is_some())
        .map(|f| format!("{base}/{f}"))
        .collect();
    if !store.files(&format!("{base}/contracts")).is_empty() {
        left.push(format!("{base}/contracts/"));
    }
    let mut out = format!("{}\n", plan.trim_end());
    if !left.is_empty() {
        out.push_str(&format!(
            "\n## Imported files\nSpec Kit files of `{name}` that stay where they are:\n"
        ));
        for path in left {
            out.push_str(&format!("- `{path}`\n"));
        }
    }
    Some(out)
}

fn spec(title: &str, name: &str, text: &str) -> String {
    let input = text
        .lines()
        .find_map(|l| {
            common::plain(l)
                .trim()
                .strip_prefix("Input:")
                .map(|i| i.trim().to_owned())
        })
        .filter(|i| !i.is_empty());
    let why = format!(
        "Imported from the Spec Kit feature `{name}`.{}",
        input.map_or(String::new(), |i| format!("\n\n{i}"))
    );
    let stories = user_stories(text);
    let functional = functional_requirements(text);
    let mut out = format!(
        "# {title}\n\n## Why\n{why}\n\n## What changes\nSee the requirements.\n\n## User stories\n"
    );
    for story in &stories {
        out.push_str(&format!("- {}\n", story.title));
    }
    out.push_str("\n## Requirements\n");
    let (mut requirement, mut criterion) = (0, 0);
    let mut add = |out: &mut String, title: &str, items: &[String]| {
        requirement += 1;
        out.push_str(&format!("\n### R{requirement}: {title}\n\n"));
        for item in items {
            criterion += 1;
            out.push_str(&format!("- C{criterion}: {item}\n"));
        }
    };
    for story in stories.iter().filter(|s| !s.scenarios.is_empty()) {
        add(&mut out, &story.title, &story.scenarios);
    }
    if !functional.is_empty() {
        add(
            &mut out,
            "Functional requirements",
            &functional
                .iter()
                .map(|f| format!("WHEN the feature is used THEN {f}"))
                .collect::<Vec<_>>(),
        );
    }
    out.push_str("\n## Out of scope\n\n## Clarifications\n\n## Open questions\n");
    for question in clarifications(text) {
        out.push_str(&format!("- {question}\n"));
    }
    out
}

struct Story {
    title: String,
    scenarios: Vec<String>,
}

/// `### User Story 1 - Title (Priority: P1)` and its numbered Given/When/Then scenarios.
fn user_stories(text: &str) -> Vec<Story> {
    let mut stories: Vec<Story> = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("### User Story") {
            let title = heading.split_once(" - ").map_or(heading, |(_, t)| t);
            let title = title.split(" (Priority").next().unwrap_or(title).trim();
            stories.push(Story {
                title: title.to_owned(),
                scenarios: Vec::new(),
            });
        } else if let Some(story) = stories.last_mut() {
            if let Some(scenario) = scenario(line) {
                story.scenarios.push(scenario);
            }
        }
    }
    stories
}

/// `1. **Given** x, **When** y, **Then** z` as `WHEN y (given x) THEN z`.
fn scenario(line: &str) -> Option<String> {
    let line = common::plain(line);
    let text = line
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ')'])
        .trim();
    let find = |word: &str| text.to_ascii_lowercase().find(word);
    let when = find("when ")?;
    let then = find("then ")?;
    if then < when {
        return None;
    }
    let given = find("given ").filter(|g| *g < when);
    let clean = |s: &str| {
        s.trim()
            .trim_matches(',')
            .trim()
            .trim_end_matches('.')
            .trim()
            .to_owned()
    };
    let when_text = clean(&text[when + 5..then]);
    let then_text = clean(&text[then + 5..]);
    Some(match given {
        Some(g) => format!(
            "WHEN {when_text} (given {}) THEN {then_text}",
            clean(&text[g + 6..when])
        ),
        None => format!("WHEN {when_text} THEN {then_text}"),
    })
}

/// `- **FR-001**: System MUST …` (clarification markers removed).
fn functional_requirements(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let plain = common::plain(line);
            let rest = plain.trim().strip_prefix("- ")?;
            let rest = rest.strip_prefix("FR-")?;
            let (_, requirement) = rest.split_once(':')?;
            let requirement = requirement
                .split("[NEEDS CLARIFICATION")
                .next()
                .unwrap_or(requirement)
                .trim()
                .trim_end_matches('.');
            (!requirement.is_empty()).then(|| format!("{requirement}."))
        })
        .collect()
}

fn clarifications(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let start = line.find("[NEEDS CLARIFICATION:")?;
            let question = line[start + "[NEEDS CLARIFICATION:".len()..]
                .split(']')
                .next()?
                .trim();
            (!question.is_empty()).then(|| question.to_owned())
        })
        .collect()
}

/// `- [ ] T001 [P] [US1] Description` becomes `- [ ] **T1** Description`, with its phase as instructions.
fn convert_tasks(text: &str) -> (String, Vec<bool>) {
    let mut out = String::from("# Tasks\n\n");
    let mut done = Vec::new();
    let mut phase = String::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            phase = heading.trim().to_owned();
        } else if let Some((ticked, rest)) = checkbox(line) {
            let mut words = rest.split_whitespace().peekable();
            if words.peek().is_some_and(|w| {
                w.starts_with('T') && w[1..].bytes().all(|b| b.is_ascii_digit()) && w.len() > 1
            }) {
                words.next();
            }
            let description: Vec<&str> = words
                .filter(|w| !(w.starts_with('[') && w.ends_with(']')))
                .collect();
            done.push(ticked);
            out.push_str(&format!(
                "- [{}] **T{}** {}\n",
                if ticked { 'x' } else { ' ' },
                done.len(),
                description.join(" ")
            ));
            if !phase.is_empty() {
                out.push_str(&format!("  Phase: {phase}\n"));
            }
        }
    }
    (out, done)
}

fn checkbox(line: &str) -> Option<(bool, &str)> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let mark = rest.chars().next()?;
    let text = rest[mark.len_utf8()..].strip_prefix("] ")?;
    match mark {
        ' ' => Some((false, text.trim())),
        'x' | 'X' => Some((true, text.trim())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Status, overview, store::MemoryStore};

    const SPEC: &str = "# Feature Specification: Photo albums\n\n**Input**: User description: \"Group shoots into albums\"\n\n## User Scenarios & Testing\n\n### User Story 1 - Create an album (Priority: P1)\n\n**Acceptance Scenarios**:\n\n1. **Given** the library is open, **When** a name is entered, **Then** the album appears.\n2. **Given** a name is taken, **When** it is entered again, **Then** nothing is created.\n\n### User Story 2 - Rename (Priority: P2)\n\n1. **Given** an album, **When** renamed, **Then** the new name shows.\n\n## Requirements\n\n### Functional Requirements\n- **FR-001**: System MUST allow creating albums.\n- **FR-002**: System MUST keep names unique [NEEDS CLARIFICATION: case sensitive?]\n";

    fn fixture() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert(
            ".specify/memory/constitution.md",
            "# Constitution\n\n### I. Test first\nAlways.\n",
        );
        store.insert("specs/001-photo-albums/spec.md", SPEC);
        store.insert(
            "specs/001-photo-albums/plan.md",
            "# Implementation Plan\nUse the library module.\n",
        );
        store.insert("specs/001-photo-albums/research.md", "# Research\nNotes.\n");
        store.insert("specs/001-photo-albums/data-model.md", "# Data\n");
        store.insert("specs/001-photo-albums/contracts/api.yaml", "x: 1\n");
        store.insert("specs/001-photo-albums/tasks.md", "# Tasks\n\n## Phase 1: Setup\n- [x] T001 [P] Create the module in src/albums\n\n## Phase 2: Stories\n- [ ] T002 [US1] Add creation\n- [ ] T003 [US2] Add renaming\n");
        store
    }

    #[test]
    fn features_and_the_constitution_are_found_but_this_layouts_own_folders_are_not() {
        let store = fixture();
        store.insert("specs/changes/0001-a/spec.md", "# A\n");
        store.insert("specs/capabilities/x/spec.md", "# X\n");
        let found = detect(&store).unwrap();
        assert_eq!(
            (found.source, found.changes, found.path.as_str()),
            (Source::SpecKit, 1, "specs")
        );
        assert!(detect(&MemoryStore::default()).is_none());
    }

    #[test]
    fn the_spec_is_rebuilt_with_stories_scenarios_and_requirements() {
        let store = fixture();
        let report = import(&store, "2026-10-24").unwrap();
        assert!(report.project);
        assert_eq!(report.changes, ["0001-photo-albums"]);
        let spec = store
            .read("specs/changes/0001-photo-albums/spec.md")
            .unwrap();
        assert!(spec.starts_with("# Photo albums\n\n## Why\nImported from the Spec Kit feature `001-photo-albums`.\n\nUser description: \"Group shoots into albums\""));
        assert!(spec.contains("## User stories\n- Create an album\n- Rename\n"));
        assert!(spec.contains("### R1: Create an album\n\n- C1: WHEN a name is entered (given the library is open) THEN the album appears\n- C2: WHEN it is entered again (given a name is taken) THEN nothing is created\n"));
        assert!(
            spec.contains("### R3: Functional requirements")
                && spec.contains(
                    "- C4: WHEN the feature is used THEN System MUST allow creating albums."
                )
        );
        assert!(spec.contains("## Open questions\n- case sensitive?\n"));
        let parsed = crate::parse::spec::parse(&spec);
        assert_eq!(parsed.requirements.len(), 3);
        assert!(parsed.requirements.iter().all(|r| !r.criteria.is_empty()));
    }

    #[test]
    fn plan_research_and_tasks_are_carried_over_and_files_without_a_place_are_listed() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let base = "specs/changes/0001-photo-albums";
        let design = store.read(&format!("{base}/design.md")).unwrap();
        assert!(
            design.contains("Use the library module.")
                && design.contains("- `specs/001-photo-albums/data-model.md`")
                && design.contains("- `specs/001-photo-albums/contracts/`")
        );
        assert!(store
            .read(&format!("{base}/research.md"))
            .unwrap()
            .contains("Notes."));
        let tasks = store.read(&format!("{base}/tasks.md")).unwrap();
        assert!(
            tasks.contains("- [x] **T1** Create the module in src/albums\n  Phase: Phase 1: Setup")
        );
        assert!(tasks.contains("- [ ] **T3** Add renaming"));
        let project = store.read("specs/project.md").unwrap();
        assert!(project.contains("## Principles\n### I. Test first"));
        let found = overview(&store);
        let change = found
            .changes
            .iter()
            .find(|c| c.dir == "0001-photo-albums")
            .unwrap();
        assert_eq!(
            (change.status, change.tasks_done, change.tasks_total),
            (Status::Building, 1, 3)
        );
    }

    #[test]
    fn importing_again_skips_what_is_already_there() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let again = import(&store, "2026-10-25").unwrap();
        assert!(!again.project && again.changes.is_empty());
        assert_eq!(again.skipped.len(), 2);
    }

    #[test]
    fn scenarios_without_a_given_and_lines_that_are_not_scenarios_are_handled() {
        assert_eq!(
            scenario("1. **When** x, **Then** y.").as_deref(),
            Some("WHEN x THEN y")
        );
        assert_eq!(scenario("Some prose"), None);
        assert_eq!(scenario("1. **Then** y **When** x"), None);
    }
}

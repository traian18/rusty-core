//! OpenSpec: `openspec/project.md`, `openspec/specs/<capability>/spec.md` and
//! `openspec/changes/<name>/{proposal.md, design.md, tasks.md, specs/<capability>/spec.md}`.
//!
//! A change's delta specs (`## ADDED / MODIFIED / REMOVED Requirements`) become requirements with a
//! `Capability:` line, so archiving the imported change merges them like any other.
use super::{common, Detected, ImportReport, Source};
use crate::{
    layout::Layout,
    store::{SpecError, SpecStore},
};

const ROOT: &str = "openspec";

pub fn detect(store: &dyn SpecStore) -> Option<Detected> {
    let capabilities = store.dirs(&format!("{ROOT}/specs")).len();
    let changes: Vec<String> = store.dirs(&format!("{ROOT}/changes"));
    if capabilities == 0
        && changes.is_empty()
        && store.read(&format!("{ROOT}/project.md")).is_none()
    {
        return None;
    }
    let archived = store.dirs(&format!("{ROOT}/changes/archive")).len();
    Some(Detected {
        source: Source::OpenSpec,
        path: ROOT.into(),
        capabilities,
        changes: changes.iter().filter(|c| *c != "archive").count(),
        archived,
    })
}

pub fn import(store: &dyn SpecStore, date: &str) -> Result<ImportReport, SpecError> {
    let layout = Layout::of(store);
    let mut report = ImportReport::default();
    if let Some(text) = store.read(&format!("{ROOT}/project.md")) {
        if store.read(&layout.project()).is_some() {
            report
                .skipped
                .push(format!("{} already exists", layout.project()));
        } else {
            store.write(
                &layout.project(),
                &common::with_principles(
                    &text,
                    "Imported from OpenSpec; list the rules every change is checked against.",
                ),
            )?;
            report.project = true;
        }
    }
    for name in store.dirs(&format!("{ROOT}/specs")) {
        let Some(text) = store.read(&format!("{ROOT}/specs/{name}/spec.md")) else {
            continue;
        };
        let target = layout.capability_spec(&name);
        if store.read(&target).is_some() {
            report
                .skipped
                .push(format!("capability {name} already exists"));
            continue;
        }
        store.write(&target, &capability(&text, &name))?;
        report.capabilities.push(name);
    }
    for name in store.dirs(&format!("{ROOT}/changes")) {
        if name == "archive" {
            continue;
        }
        let marker = format!("openspec/changes/{name}");
        if common::imported_before(store, &marker) {
            report
                .skipped
                .push(format!("change {name} was imported before"));
            continue;
        }
        let dir = change(store, &name, &marker, date)?;
        report.changes.push(dir);
    }
    Ok(report)
}

/// A capability spec: OpenSpec's `## Purpose` section becomes the `Purpose:` line this layout reads.
fn capability(text: &str, name: &str) -> String {
    let purpose = common::sections(text)
        .into_iter()
        .find(|(h, _)| h == "purpose")
        .map(|(_, lines)| common::body(&lines))
        .filter(|p| !p.is_empty());
    let Some(purpose) = purpose else {
        return format!("{}\n", text.trim_end());
    };
    let first = purpose.lines().next().unwrap_or_default().trim().to_owned();
    let mut out = Vec::new();
    let mut titled = false;
    for line in text.lines() {
        out.push(line.to_owned());
        if !titled && line.starts_with("# ") {
            titled = true;
            out.push(String::new());
            out.push(format!("Purpose: {first}"));
        }
    }
    if !titled {
        out.insert(
            0,
            format!("# {}\n\nPurpose: {first}", common::humanize(name)),
        );
    }
    format!("{}\n", out.join("\n"))
}

enum Kind {
    Added,
    Modified,
    Removed,
}

struct Delta {
    capability: String,
    kind: Kind,
    name: String,
    prose: Vec<String>,
    scenarios: Vec<String>,
}

fn change(
    store: &dyn SpecStore,
    name: &str,
    marker: &str,
    date: &str,
) -> Result<String, SpecError> {
    let base = format!("{ROOT}/changes/{name}");
    let proposal = store
        .read(&format!("{base}/proposal.md"))
        .unwrap_or_default();
    let sections = common::sections(&proposal);
    let section = |heading: &str| {
        sections
            .iter()
            .find(|(h, _)| h == heading)
            .map(|(_, lines)| common::body(lines))
            .filter(|b| !b.is_empty())
    };
    let title = proposal
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().trim_start_matches("Change:").trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| common::humanize(name));
    let mut deltas = Vec::new();
    for capability in store.dirs(&format!("{base}/specs")) {
        if let Some(text) = store.read(&format!("{base}/specs/{capability}/spec.md")) {
            deltas.extend(parse_deltas(&capability, &text));
        }
    }
    let tasks = store
        .read(&format!("{base}/tasks.md"))
        .map(|t| convert_tasks(&t));
    let done: Vec<bool> = tasks
        .as_ref()
        .map(|(_, done)| done.clone())
        .unwrap_or_default();
    let mut files = vec![(
        "spec.md",
        spec(
            &title,
            name,
            section("why"),
            section("what changes"),
            &deltas,
        ),
    )];
    if let Some(design) = store.read(&format!("{base}/design.md")) {
        files.push(("design.md", format!("{}\n", design.trim_end())));
    }
    if let Some((text, _)) = tasks {
        files.push(("tasks.md", text));
    }
    let status = common::status_from(true, &done);
    common::write_change(
        store,
        &common::Converted {
            title: &title,
            name,
            status,
            date,
            marker,
            files,
        },
    )
}

fn parse_deltas(capability: &str, text: &str) -> Vec<Delta> {
    let mut deltas: Vec<Delta> = Vec::new();
    let mut kind: Option<Kind> = None;
    let mut in_scenario = false;
    let mut current: Vec<String> = Vec::new();
    let flush = |deltas: &mut Vec<Delta>, current: &mut Vec<String>| {
        if let Some(last) = deltas.last_mut() {
            if !current.is_empty() {
                last.scenarios.push(current.join(" "));
            }
        }
        current.clear();
    };
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            flush(&mut deltas, &mut current);
            in_scenario = false;
            let word = heading.trim().to_ascii_lowercase();
            kind = if word.starts_with("added") {
                Some(Kind::Added)
            } else if word.starts_with("modified") {
                Some(Kind::Modified)
            } else if word.starts_with("removed") {
                Some(Kind::Removed)
            } else {
                None
            };
        } else if let (Some(name), Some(kind)) =
            (line.strip_prefix("### Requirement:"), kind.as_ref())
        {
            flush(&mut deltas, &mut current);
            in_scenario = false;
            let kind = match kind {
                Kind::Added => Kind::Added,
                Kind::Modified => Kind::Modified,
                Kind::Removed => Kind::Removed,
            };
            deltas.push(Delta {
                capability: capability.to_owned(),
                kind,
                name: name.trim().to_owned(),
                prose: Vec::new(),
                scenarios: Vec::new(),
            });
        } else if line.starts_with("#### Scenario") {
            flush(&mut deltas, &mut current);
            in_scenario = true;
        } else if let Some(last) = deltas.last_mut() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if in_scenario {
                let bullet = trimmed.trim_start_matches(['-', '*']).trim();
                current.push(common::plain(bullet));
            } else {
                last.prose.push(trimmed.to_owned());
            }
        }
    }
    flush(&mut deltas, &mut current);
    deltas
}

fn spec(
    title: &str,
    name: &str,
    why: Option<String>,
    what: Option<String>,
    deltas: &[Delta],
) -> String {
    let mut out = format!(
        "# {title}\n\n## Why\n{}\n\n## What changes\n{}\n\n## User stories\n\n## Requirements\n",
        why.unwrap_or_else(|| format!("Imported from the OpenSpec change `{name}`.")),
        what.unwrap_or_else(|| "See the requirements.".into())
    );
    let mut criterion = 0;
    for (index, delta) in deltas.iter().enumerate() {
        let action = match delta.kind {
            Kind::Added => "adds",
            Kind::Modified => "changes",
            Kind::Removed => "removes",
        };
        out.push_str(&format!(
            "\n### R{}: {}\nCapability: {} ({action} \"{}\")\n",
            index + 1,
            delta.name,
            delta.capability,
            delta.name
        ));
        if !delta.prose.is_empty() {
            out.push_str(&format!("{}\n", delta.prose.join("\n")));
        }
        out.push('\n');
        let mut scenarios = delta.scenarios.clone();
        if scenarios.is_empty() {
            scenarios.push(match delta.kind {
                Kind::Removed => format!(
                    "WHEN the system is used THEN \"{}\" is no longer provided.",
                    delta.name
                ),
                _ => format!("WHEN the system is used THEN \"{}\" holds.", delta.name),
            });
        }
        for scenario in scenarios {
            criterion += 1;
            out.push_str(&format!("- C{criterion}: {scenario}\n"));
        }
    }
    out.push_str("\n## Out of scope\n\n## Clarifications\n\n## Open questions\n");
    out
}

/// `## 1. Group` headings and `- [ ] 1.1 Text` items become `- [ ] **T1** Text` with the group as instructions.
fn convert_tasks(text: &str) -> (String, Vec<bool>) {
    let mut out = String::from("# Tasks\n\n");
    let mut done = Vec::new();
    let mut group = String::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            group = heading
                .trim()
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
                .trim()
                .to_owned();
        } else if let Some((ticked, rest)) = checkbox(line) {
            let title = rest
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
                .trim();
            done.push(ticked);
            out.push_str(&format!(
                "- [{}] **T{}** {title}\n",
                if ticked { 'x' } else { ' ' },
                done.len()
            ));
            if !group.is_empty() {
                out.push_str(&format!("  Group: {group}\n"));
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
    use crate::{model::Status, overview, store::MemoryStore, validate};

    fn fixture() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert(
            "openspec/project.md",
            "# Project Context\n\n## Purpose\nPhotos.\n",
        );
        store.insert("openspec/specs/albums/spec.md", "# albums Specification\n\n## Purpose\nGrouping photos without moving them.\n\n## Requirements\n\n### Requirement: Album creation\nThe system SHALL create albums.\n\n#### Scenario: New name\n- **WHEN** a new name is given\n- **THEN** the album appears\n");
        store.insert("openspec/changes/add-rename/proposal.md", "# Change: Rename albums\n\n## Why\nPeople mistype names.\n\n## What Changes\n- Albums can be renamed\n\n## Impact\n- albums\n");
        store.insert("openspec/changes/add-rename/specs/albums/spec.md", "## ADDED Requirements\n### Requirement: Album renaming\nThe system SHALL rename albums.\n\n#### Scenario: Rename\n- **WHEN** a new name is given\n- **THEN** the album is renamed\n- **AND** the old name is free\n\n## MODIFIED Requirements\n### Requirement: Album creation\nNames are unique.\n\n#### Scenario: Taken\n- **WHEN** the name is taken\n- **THEN** nothing is created\n\n## REMOVED Requirements\n### Requirement: Album export\n");
        store.insert(
            "openspec/changes/add-rename/design.md",
            "# Design\nSmall.\n",
        );
        store.insert("openspec/changes/add-rename/tasks.md", "## 1. Model\n- [x] 1.1 Add rename to the model\n- [ ] 1.2 Add the sidebar action\n\n## 2. Tests\n- [ ] 2.1 Test renaming\n");
        store.insert(
            "openspec/changes/archive/2025-01-01-old/proposal.md",
            "# Old\n",
        );
        store
    }

    #[test]
    fn it_is_detected_with_counts() {
        let found = detect(&fixture()).unwrap();
        assert_eq!(
            (found.capabilities, found.changes, found.archived),
            (1, 1, 1)
        );
        assert!(detect(&MemoryStore::default()).is_none());
    }

    #[test]
    fn project_capabilities_and_changes_are_converted() {
        let store = fixture();
        let report = import(&store, "2026-10-24").unwrap();
        assert!(report.project);
        assert_eq!(report.capabilities, ["albums"]);
        assert_eq!(report.changes, ["0001-add-rename"]);
        assert!(store
            .read("specs/project.md")
            .unwrap()
            .contains("## Principles"));
        let capability = store.read("specs/capabilities/albums/spec.md").unwrap();
        assert!(capability.starts_with(
            "# albums Specification\n\nPurpose: Grouping photos without moving them.\n"
        ));
        assert!(capability.contains("### Requirement: Album creation"));
    }

    #[test]
    fn deltas_become_requirements_that_archive_can_merge() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let spec = store.read("specs/changes/0001-add-rename/spec.md").unwrap();
        assert!(spec.starts_with("# Rename albums\n\n## Why\nPeople mistype names."));
        assert!(spec.contains("### R1: Album renaming\nCapability: albums (adds \"Album renaming\")\nThe system SHALL rename albums."));
        assert!(spec.contains(
            "- C1: WHEN a new name is given THEN the album is renamed AND the old name is free"
        ));
        assert!(
            spec.contains("Capability: albums (changes \"Album creation\")")
                && spec.contains("Capability: albums (removes \"Album export\")")
        );
        assert!(spec.contains("THEN \"Album export\" is no longer provided."));
        let parsed = crate::parse::spec::parse(&spec);
        assert_eq!(parsed.requirements.len(), 3);
        assert!(parsed.requirements.iter().all(|r| !r.criteria.is_empty()));
    }

    #[test]
    fn tasks_keep_their_ticks_and_groups_and_the_status_follows() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let tasks = store
            .read("specs/changes/0001-add-rename/tasks.md")
            .unwrap();
        assert!(tasks.contains("- [x] **T1** Add rename to the model\n  Group: Model"));
        assert!(tasks.contains("- [ ] **T3** Test renaming\n  Group: Tests"));
        let found = overview(&store);
        assert_eq!(
            (
                found.changes[0].status,
                found.changes[0].tasks_done,
                found.changes[0].tasks_total
            ),
            (Status::Building, 1, 3)
        );
        assert!(store
            .read("specs/changes/0001-add-rename/design.md")
            .unwrap()
            .contains("Small."));
        let warnings = validate::validate_change(
            &store,
            &Layout::new("specs"),
            "0001-add-rename",
            Status::Building,
        );
        assert!(warnings
            .iter()
            .all(|w| w.severity != crate::model::Severity::Error));
    }

    #[test]
    fn importing_twice_changes_nothing_and_says_why() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let again = import(&store, "2026-10-25").unwrap();
        assert!(!again.project && again.capabilities.is_empty() && again.changes.is_empty());
        assert_eq!(again.skipped.len(), 3);
        assert_eq!(store.dirs("specs/changes").len(), 1);
    }

    #[test]
    fn an_imported_change_can_be_archived_into_the_capabilities() {
        let store = fixture();
        import(&store, "2026-10-24").unwrap();
        let done = crate::archive::archive(&store, "0001-add-rename", "2026-10-30", true);
        // "Album export" is not in the imported capability, so removing it cannot be merged.
        assert!(done
            .unwrap_err()
            .message
            .contains("no requirement named \"Album export\""));
    }
}

//! To OpenSpec: `openspec/project.md`, `openspec/specs/<capability>/spec.md`, and per change
//! `openspec/changes/<slug>/{proposal.md, design.md, tasks.md, specs/<capability>/spec.md}`
//! (archived ones under `changes/archive/<date>-<slug>/`).
//!
//! A requirement's `Capability:` line says which delta it is: adds go under `## ADDED Requirements`,
//! changes under `## MODIFIED`, removals under `## REMOVED`. Requirements with no such line belong
//! to no capability and are not written.
use std::collections::BTreeMap;

use super::{note_export, scenario, source, write_new, ExportReport};
use crate::{
    layout::Layout,
    model::Requirement,
    parse::spec::{capability_ref, Action},
    store::{SpecError, SpecStore},
};

const ROOT: &str = "openspec";

pub fn export(store: &dyn SpecStore, date: &str) -> Result<ExportReport, SpecError> {
    let layout = Layout::of(store);
    let mut report = ExportReport::default();
    if let Some(project) = store.read(&layout.project()) {
        write_new(
            store,
            &mut report,
            &format!("{ROOT}/project.md"),
            &format!("{}\n", project.trim_end()),
        )?;
    }
    for name in store.dirs(&layout.capabilities()) {
        if let Some(text) = store.read(&layout.capability_spec(&name)) {
            write_new(
                store,
                &mut report,
                &format!("{ROOT}/specs/{name}/spec.md"),
                &capability(&text, &name),
            )?;
        }
    }
    for change in source::changes(store, &layout) {
        let base = if change.archived {
            format!("{ROOT}/changes/archive/{}", archive_name(&change))
        } else {
            format!("{ROOT}/changes/{}", change.slug)
        };
        if store.read(&format!("{base}/proposal.md")).is_some() {
            report.skipped.push(format!("{base} already exists"));
            continue;
        }
        write_change(store, &mut report, &base, &change)?;
        note_export(store, &layout, &change, &base, date)?;
    }
    Ok(report)
}

fn archive_name(change: &source::ChangeDoc) -> String {
    let date = if change.created.is_empty() {
        "0000-00-00"
    } else {
        &change.created
    };
    format!("{date}-{}", change.slug)
}

/// Our capability spec in OpenSpec's shape: `## Purpose`, `## Requirements`, scenario blocks.
fn capability(text: &str, name: &str) -> String {
    let mut out = Vec::new();
    let mut in_scenarios = false;
    let mut purpose = None;
    for line in text.lines() {
        if let Some(p) = line.trim().strip_prefix("Purpose:") {
            purpose = Some(p.trim().to_owned());
            continue;
        }
        if line.starts_with("#### Scenarios") {
            in_scenarios = true;
            continue;
        }
        if line.starts_with('#') {
            in_scenarios = false;
        }
        if in_scenarios {
            if let Some(bullet) = line.trim().strip_prefix("- ") {
                out.extend(scenario_block(bullet));
            }
            continue;
        }
        if line.trim_start().starts_with("_Changed by ") {
            continue;
        }
        out.push(line.to_owned());
    }
    let mut text = out.join("\n");
    if let Some(purpose) = purpose.filter(|p| !p.is_empty()) {
        // After the title: OpenSpec keeps the purpose in its own section.
        let (head, rest) = text.split_once('\n').unwrap_or((&text, ""));
        text = format!("{head}\n\n## Purpose\n{purpose}\n{rest}");
    } else if !text.starts_with('#') {
        text = format!("# {name} Specification\n\n{text}");
    }
    let compact: Vec<&str> = text.lines().fold(Vec::new(), |mut acc, line| {
        if !(line.trim().is_empty() && acc.last().is_some_and(|l: &&str| l.trim().is_empty())) {
            acc.push(line);
        }
        acc
    });
    format!("{}\n", compact.join("\n").trim_end())
}

/// One scenario as OpenSpec writes it.
fn scenario_block(text: &str) -> Vec<String> {
    let s = scenario(text);
    let name: Vec<&str> = s.when.split_whitespace().take(6).collect();
    let mut lines = vec![
        String::new(),
        format!("#### Scenario: {}", capitalize(&name.join(" "))),
    ];
    if let Some(given) = &s.given {
        lines.push(format!("- **GIVEN** {given}"));
    }
    lines.push(format!("- **WHEN** {}", s.when));
    lines.push(format!("- **THEN** {}", s.then));
    lines.extend(s.and.iter().map(|a| format!("- **AND** {a}")));
    lines
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or(String::new(), |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn write_change(
    store: &dyn SpecStore,
    report: &mut ExportReport,
    base: &str,
    change: &source::ChangeDoc,
) -> Result<(), SpecError> {
    let mut deltas: BTreeMap<String, [Vec<String>; 3]> = BTreeMap::new();
    let mut unlisted = Vec::new();
    for requirement in &change.requirements {
        let Some(reference) = requirement.capability.as_deref().and_then(capability_ref) else {
            unlisted.push(requirement.title.clone());
            continue;
        };
        let name = reference
            .requirement
            .clone()
            .unwrap_or_else(|| requirement.title.clone());
        let slot = match reference.action {
            Action::Adds => 0,
            Action::Changes => 1,
            Action::Removes => 2,
        };
        deltas.entry(reference.capability).or_default()[slot].extend(requirement_block(
            &name,
            requirement,
            reference.action,
        ));
    }
    let affected: Vec<&String> = deltas.keys().collect();
    let proposal = format!(
        "# Change: {}\n\n## Why\n{}\n\n## What Changes\n{}\n\n## Impact\n- Affected specs: {}\n",
        change.title,
        or(&change.why, "See the requirements."),
        or(&change.what, "See the requirements."),
        if affected.is_empty() {
            "none".to_owned()
        } else {
            affected
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    write_new(store, report, &format!("{base}/proposal.md"), &proposal)?;
    for (capability, [added, modified, removed]) in &deltas {
        let mut text = String::new();
        for (heading, blocks) in [
            ("ADDED", added),
            ("MODIFIED", modified),
            ("REMOVED", removed),
        ] {
            if !blocks.is_empty() {
                text.push_str(&format!(
                    "## {heading} Requirements\n{}\n",
                    blocks.join("\n")
                ));
            }
        }
        write_new(
            store,
            report,
            &format!("{base}/specs/{capability}/spec.md"),
            &text,
        )?;
    }
    if let Some(design) = &change.design {
        write_new(
            store,
            report,
            &format!("{base}/design.md"),
            &format!("{}\n", design.trim_end()),
        )?;
    }
    if !change.tasks.is_empty() {
        let mut tasks = String::from("## 1. Implementation\n");
        for (index, task) in change.tasks.iter().enumerate() {
            tasks.push_str(&format!(
                "- [{}] 1.{} {}\n",
                if task.done { 'x' } else { ' ' },
                index + 1,
                task.title
            ));
        }
        write_new(store, report, &format!("{base}/tasks.md"), &tasks)?;
    }
    if !unlisted.is_empty() {
        report.skipped.push(format!(
            "{}: requirement{} without a Capability: line not written ({})",
            change.dir,
            if unlisted.len() == 1 { "" } else { "s" },
            unlisted.join(", ")
        ));
    }
    Ok(())
}

fn or<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.trim().is_empty() {
        fallback
    } else {
        text
    }
}

/// `### Requirement: Name`, the statement (OpenSpec wants SHALL or MUST), and its scenarios.
fn requirement_block(name: &str, requirement: &Requirement, action: Action) -> Vec<String> {
    let mut lines = vec![String::new(), format!("### Requirement: {name}")];
    if action == Action::Removes {
        lines.push("**Reason**: Removed by this change.".into());
        return lines;
    }
    let statement = if requirement.detail.trim().is_empty() {
        requirement.title.clone()
    } else {
        requirement.detail.clone()
    };
    let upper = statement.to_ascii_uppercase();
    lines.push(if upper.contains("SHALL") || upper.contains("MUST") {
        statement
    } else {
        format!("The system SHALL provide: {statement}")
    });
    for criterion in &requirement.criteria {
        lines.extend(scenario_block(&criterion.text));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{import, parse, store::MemoryStore};

    fn project() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/project.md", "# Project\n\n## Principles\n- small\n");
        store.insert("specs/capabilities/albums/spec.md", "# Albums\n\nPurpose: grouping photos.\n\n## Requirements\n\n### Requirement: Album creation\nThe system SHALL create albums.\n\n#### Scenarios\n- WHEN a new name is given THEN the album appears AND the name is used.\n- WHEN the name is taken THEN nothing is created.\n\n_Changed by 0001-a (2026-10-01)._\n");
        store.insert("specs/changes/0007-rename/change.md", "---\nid: \"0007\"\ntitle: Rename albums\nstatus: building\ncreated: 2026-10-22\n---\n# Rename albums\n");
        store.insert("specs/changes/0007-rename/spec.md", "# Rename albums\n\n## Why\nPeople mistype names.\n\n## What changes\nAlbums can be renamed.\n\n## Requirements\n\n### R1: Renaming\nCapability: albums (adds \"Album renaming\")\nAlbums can be renamed.\n\n- C1: WHEN a name is given THEN the album is renamed.\n\n### R2: Naming rules\nCapability: albums (changes \"Album creation\")\nNames are unique.\n\n- C2: WHEN the name is taken THEN nothing is created.\n\n### R3: Old export\nCapability: albums (removes \"Album export\")\n\n- C3: WHEN exporting THEN it is gone.\n\n### R4: Internal\n- C4: WHEN x THEN y.\n");
        store.insert("specs/changes/0007-rename/design.md", "# Design\nSmall.\n");
        store.insert(
            "specs/changes/0007-rename/tasks.md",
            "- [x] **T1** Model. R1 (C1)\n- [ ] **T2** Sidebar. R1 (C1)\n  Wire it.\n",
        );
        store.insert(
            "specs/archive/2026-09-01-0003-old/change.md",
            "---\ntitle: Old\ncreated: 2026-08-30\n---\n",
        );
        store.insert("specs/archive/2026-09-01-0003-old/spec.md", "# Old\n\n## Requirements\n### R1: X\nCapability: albums (adds \"X\")\n- C1: WHEN a THEN b.\n");
        store
    }

    #[test]
    fn project_and_capabilities_are_written_in_openspec_shape() {
        let store = project();
        let report = export(&store, "2026-10-30").unwrap();
        assert!(store
            .read("openspec/project.md")
            .unwrap()
            .contains("## Principles"));
        let capability = store.read("openspec/specs/albums/spec.md").unwrap();
        assert!(capability.starts_with("# Albums\n\n## Purpose\ngrouping photos.\n"));
        assert!(capability.contains("#### Scenario: A new name is given\n- **WHEN** a new name is given\n- **THEN** the album appears\n- **AND** the name is used"));
        assert!(!capability.contains("_Changed by"));
        assert!(report
            .written
            .contains(&"openspec/specs/albums/spec.md".to_owned()));
    }

    #[test]
    fn a_change_becomes_a_proposal_with_delta_specs_design_and_tasks() {
        let store = project();
        let report = export(&store, "2026-10-30").unwrap();
        let proposal = store.read("openspec/changes/rename/proposal.md").unwrap();
        assert!(proposal.starts_with("# Change: Rename albums\n\n## Why\nPeople mistype names.\n\n## What Changes\nAlbums can be renamed.\n\n## Impact\n- Affected specs: albums"));
        let delta = store
            .read("openspec/changes/rename/specs/albums/spec.md")
            .unwrap();
        assert!(
            delta.contains(
                "## ADDED Requirements\n\n### Requirement: Album renaming\nAlbums can be renamed."
            ) || delta.contains(
                "### Requirement: Album renaming\nThe system SHALL provide: Albums can be renamed."
            )
        );
        assert!(
            delta.contains("## MODIFIED Requirements")
                && delta.contains(
                    "### Requirement: Album creation\nThe system SHALL provide: Names are unique."
                )
        );
        assert!(delta.contains("## REMOVED Requirements\n\n### Requirement: Album export\n**Reason**: Removed by this change."));
        assert!(store
            .read("openspec/changes/rename/design.md")
            .unwrap()
            .contains("Small."));
        let tasks = store.read("openspec/changes/rename/tasks.md").unwrap();
        assert!(tasks.contains("## 1. Implementation\n- [x] 1.1 Model\n- [ ] 1.2 Sidebar"));
        assert!(report
            .skipped
            .iter()
            .any(|s| s.contains("without a Capability: line") && s.contains("Internal")));
        assert!(store
            .read("openspec/changes/archive/2026-08-30-old/proposal.md")
            .is_some());
    }

    #[test]
    fn nothing_that_exists_is_overwritten() {
        let store = project();
        store.insert("openspec/project.md", "mine");
        store.insert("openspec/changes/rename/proposal.md", "mine");
        let report = export(&store, "2026-10-30").unwrap();
        assert_eq!(store.read("openspec/project.md").unwrap(), "mine");
        assert_eq!(
            store.read("openspec/changes/rename/proposal.md").unwrap(),
            "mine"
        );
        assert!(report
            .skipped
            .iter()
            .any(|s| s.contains("openspec/project.md already exists")));
        assert!(report
            .skipped
            .iter()
            .any(|s| s.contains("openspec/changes/rename already exists")));
    }

    #[test]
    fn what_is_exported_imports_back_into_the_same_requirements() {
        let store = project();
        export(&store, "2026-10-30").unwrap();
        let back = MemoryStore::default();
        for path in store
            .paths()
            .into_iter()
            .filter(|p| p.starts_with("openspec/"))
        {
            back.insert(&path, &store.read(&path).unwrap());
        }
        import::import(&back, import::Source::OpenSpec, "2026-10-30").unwrap();
        let capability =
            parse::capability::parse(&back.read("specs/capabilities/albums/spec.md").unwrap());
        assert_eq!(capability.requirements, ["Album creation"]);
        let spec = parse::spec::parse(&back.read("specs/changes/0001-rename/spec.md").unwrap());
        let refs: Vec<String> = spec
            .requirements
            .iter()
            .map(|r| r.capability.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            refs,
            [
                "albums (adds \"Album renaming\")",
                "albums (changes \"Album creation\")",
                "albums (removes \"Album export\")"
            ]
        );
        assert!(spec.requirements[0].criteria[0]
            .text
            .contains("WHEN a name is given THEN the album is renamed"));
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
        let report = import::import(&store, import::Source::OpenSpec, "2026-10-31").unwrap();
        assert!(report.changes.is_empty(), "{report:?}");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("rename") && s.contains("imported before")),
            "{report:?}"
        );
        assert_eq!(store.dirs("specs/changes").len(), before);
    }
}

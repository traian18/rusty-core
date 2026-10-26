//! The text a spec workflow starts from: the project, the capabilities and the change's own
//! documents, composed fresh from the files when the run starts.
use crate::{
    layout::{parse_dir_name, Layout},
    parse,
    record::Step,
    store::SpecStore,
};

/// Longest a single document may be in the context; the rest is cut with a note.
const MAX_DOCUMENT_CHARS: usize = 24_000;

/// The context for `step` on change `dir`. Specify sees the project and the capabilities; Plan
/// also sees the spec and what an earlier Plan wrote (to revise it, not start over); Implement
/// sees the spec, the design and the research behind them.
pub fn compose(store: &dyn SpecStore, dir: &str, step: Step) -> String {
    let mut out = compose_documents(store, dir, step);
    append_instructions(&mut out, store, step);
    out
}

fn step_file(step: Step) -> &'static str {
    match step {
        Step::Specify => "specify",
        Step::Plan => "plan",
        Step::Implement => "implement",
        Step::Start => "start",
        Step::Capability => "capability",
    }
}

/// The project's own rules for a step: `<root>/instructions/all.md` and `<step>.md`, written by
/// the team and given to the step after its documents, as additions to its built-in instructions.
fn append_instructions(out: &mut String, store: &dyn SpecStore, step: Step) {
    let layout = Layout::of(store);
    for name in ["all", step_file(step)] {
        section(
            out,
            &format!("This project's instructions for this step ({name}.md); follow them in addition to yours"),
            &format!("{}/instructions/{name}.md", layout.root),
            store,
        );
    }
}

fn compose_documents(store: &dyn SpecStore, dir: &str, step: Step) -> String {
    match step {
        Step::Start => return compose_start(store),
        // `dir` is the capability's folder name.
        Step::Capability => return compose_capability(store, dir),
        _ => {}
    }
    let layout = Layout::of(store);
    let base = layout.change(dir);
    let number = parse_dir_name(dir).map_or(String::new(), |(n, _)| format!("{n:04} "));
    let mut out = format!("This run works on the change {number}in the folder {base}/.\n");
    section(&mut out, "Project", &layout.project(), store);
    out.push_str(&capability_list(store, &layout));
    section(
        &mut out,
        "Change record",
        &format!("{base}/change.md"),
        store,
    );
    let documents: &[(&str, &str)] = match step {
        Step::Specify => &[
            (
                "Current spec.md (revise it; keep the ids and what the person wrote)",
                "spec.md",
            ),
            (
                "Current checklist.md (keep the ids and the items the person ticked)",
                "checklist.md",
            ),
        ],
        Step::Start | Step::Capability => &[],
        Step::Implement => &[
            ("The spec being built", "spec.md"),
            ("The design to follow", "design.md"),
            (
                "What research found (where things are in the code, with file:line evidence)",
                "research.md",
            ),
        ],
        Step::Plan => &[
            ("The spec to design and plan for", "spec.md"),
            (
                "The spec's quality checklist (open items are gaps the design must handle or name)",
                "checklist.md",
            ),
            ("Current research.md (revise it)", "research.md"),
            (
                "Current design.md (revise it; keep what the person wrote)",
                "design.md",
            ),
            (
                "Current tasks.md (keep ticked tasks and their ids)",
                "tasks.md",
            ),
        ],
    };
    for (title, name) in documents {
        section(&mut out, title, &format!("{base}/{name}"), store);
    }
    out
}

/// What Start specs revises: the project file and every capability spec that exists.
fn compose_start(store: &dyn SpecStore) -> String {
    let layout = Layout::of(store);
    let mut out = format!("This run writes the project file and the capability specs of this repository, under {}/.\n", layout.root);
    section(
        &mut out,
        "Current project file (revise it; keep what the person wrote)",
        &layout.project(),
        store,
    );
    for name in store.dirs(&layout.capabilities()) {
        let path = layout.capability_spec(&name);
        section(
            &mut out,
            &format!("Current capability {name} (revise it; keep what the person wrote)"),
            &path,
            store,
        );
    }
    out
}

/// What one capability's spec is written from: the project, its entry in the survey's list, the
/// other capabilities (so their ground is not covered twice) and the current spec to revise.
fn compose_capability(store: &dyn SpecStore, name: &str) -> String {
    let layout = Layout::of(store);
    let entries = crate::onboarding::read(store, &layout).unwrap_or_default();
    let mut out = format!(
        "This run writes the spec of the capability `{name}`, into {}.\n",
        layout.capability_spec(name)
    );
    section(&mut out, "Project", &layout.project(), store);
    if let Some(entry) = entries.iter().find(|e| e.name == name) {
        out.push_str(&format!(
            "\n## This capability (from {})\n\nName: {}\n",
            crate::onboarding::path(&layout),
            entry.title
        ));
        if !entry.purpose.is_empty() {
            out.push_str(&format!("Purpose: {}\n", entry.purpose));
        }
        if !entry.paths.is_empty() {
            out.push_str(&format!("Start reading at: {}\n", entry.paths.join(", ")));
        }
        if !entry.notes.is_empty() {
            out.push_str(&format!("Notes from the person: {}\n", entry.notes));
        }
    }
    let others: Vec<String> = entries
        .iter()
        .filter(|e| e.name != name)
        .map(|e| format!("- {}: {}", e.name, e.purpose))
        .collect();
    if !others.is_empty() {
        out.push_str(&format!(
            "\n## The other capabilities of this project (do not cover their ground)\n\n{}\n",
            others.join("\n")
        ));
    }
    section(
        &mut out,
        "Current spec (revise it; keep what the person wrote)",
        &layout.capability_spec(name),
        store,
    );
    out
}

fn section(out: &mut String, title: &str, path: &str, store: &dyn SpecStore) {
    let Some(text) = store.read(path) else { return };
    push_document(out, &format!("{title} ({path})"), &text);
}

/// A document under its own heading, cut at [`MAX_DOCUMENT_CHARS`] with a note.
pub(crate) fn push_document(out: &mut String, heading: &str, text: &str) {
    out.push_str(&format!("\n## {heading}\n\n"));
    if text.chars().count() > MAX_DOCUMENT_CHARS {
        out.extend(text.chars().take(MAX_DOCUMENT_CHARS));
        out.push_str("\n[… the rest of this document is omitted]\n");
    } else {
        out.push_str(text.trim_end());
        out.push('\n');
    }
}

fn capability_list(store: &dyn SpecStore, layout: &Layout) -> String {
    let names = store.dirs(&layout.capabilities());
    if names.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Capabilities that already exist\n\n");
    for name in names {
        let doc = store
            .read(&layout.capability_spec(&name))
            .map(|t| parse::capability::parse(&t))
            .unwrap_or_default();
        let purpose = doc.purpose.map_or(String::new(), |p| format!(": {p}"));
        out.push_str(&format!(
            "- {name}{purpose} ({} requirements, {})\n",
            doc.requirements.len(),
            layout.capability_spec(&name)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn seeded() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/project.md", "# P\n\n## Principles\n- small\n");
        store.insert(
            "specs/capabilities/albums/spec.md",
            "# Albums\n\nPurpose: grouping.\n\n### Requirement: Creation\n",
        );
        store.insert(
            "specs/changes/0002-dark/change.md",
            "---\nstatus: draft\n---\n",
        );
        store.insert("specs/changes/0002-dark/spec.md", "# Dark mode\n");
        store.insert("specs/changes/0002-dark/design.md", "# Old design\n");
        store
    }

    #[test]
    fn specify_sees_the_project_capabilities_and_its_own_spec_only() {
        let text = compose(&seeded(), "0002-dark", Step::Specify);
        assert!(text.contains("change 0002 in the folder specs/changes/0002-dark/"));
        assert!(text.contains("- small") && text.contains("- albums: grouping. (1 requirements"));
        assert!(text.contains("Current spec.md") && !text.contains("Old design"));
    }

    #[test]
    fn specify_revises_the_checklist_and_plan_reads_it() {
        let store = seeded();
        store.insert(
            "specs/changes/0002-dark/checklist.md",
            "- [ ] CHK001 Is the contrast given?\n",
        );
        let specify = compose(&store, "0002-dark", Step::Specify);
        let plan = compose(&store, "0002-dark", Step::Plan);
        let implement = compose(&store, "0002-dark", Step::Implement);
        assert!(specify.contains("Current checklist.md") && specify.contains("CHK001"));
        assert!(plan.contains("quality checklist") && plan.contains("CHK001"));
        assert!(!implement.contains("CHK001"));
    }

    #[test]
    fn implement_sees_the_spec_and_the_design_but_not_the_old_tasks() {
        let store = seeded();
        store.insert("specs/changes/0002-dark/tasks.md", "- [ ] **T1** x. R1\n");
        let text = compose(&store, "0002-dark", Step::Implement);
        assert!(
            text.contains("The spec being built")
                && text.contains("# Old design")
                && !text.contains("**T1**")
        );
    }

    #[test]
    fn implement_sees_the_research_after_the_design() {
        let store = seeded();
        store.insert(
            "specs/changes/0002-dark/research.md",
            "Themes load in src/theme.ts:12.\n",
        );
        let text = compose(&store, "0002-dark", Step::Implement);
        assert!(text.contains("What research found") && text.contains("src/theme.ts:12"));
        assert!(text.find("# Old design") < text.find("src/theme.ts:12"));
    }

    #[test]
    fn start_sees_the_project_and_every_capability_but_no_change() {
        let text = compose(&seeded(), "", Step::Start);
        assert!(text.contains("Current project file") && text.contains("- small"));
        assert!(text.contains("Current capability albums") && text.contains("Purpose: grouping."));
        assert!(!text.contains("Dark mode"));
    }

    #[test]
    fn a_capability_run_sees_its_entry_the_others_and_its_current_spec() {
        let store = seeded();
        store.insert("specs/onboarding.md", "# O\n\n## albums\nPurpose: grouping.\nPaths: `src/albums/`\nNotes: names are unique\n\n## import\nPurpose: bringing photos in\n");
        let text = compose(&store, "albums", Step::Capability);
        assert!(text.contains("the capability `albums`, into specs/capabilities/albums/spec.md"));
        assert!(
            text.contains("Start reading at: src/albums/")
                && text.contains("Notes from the person: names are unique")
        );
        assert!(text.contains("- import: bringing photos in") && !text.contains("- albums:"));
        assert!(text.contains("Current spec (revise it") && text.contains("Purpose: grouping."));
        assert!(text.contains("- small"));
    }

    #[test]
    fn the_projects_own_instructions_follow_the_documents() {
        let store = seeded();
        store.insert("specs/instructions/all.md", "Always note rollback.");
        store.insert("specs/instructions/plan.md", "Use ADR style.");
        let plan = compose(&store, "0002-dark", Step::Plan);
        assert!(plan.contains("Always note rollback.") && plan.contains("Use ADR style."));
        assert!(plan.find("# Old design") < plan.find("Always note rollback."));
        let specify = compose(&store, "0002-dark", Step::Specify);
        assert!(specify.contains("Always note rollback.") && !specify.contains("ADR"));
    }

    #[test]
    fn plan_also_sees_the_earlier_design() {
        let text = compose(&seeded(), "0002-dark", Step::Plan);
        assert!(text.contains("The spec to design and plan for") && text.contains("# Old design"));
        assert!(!text.contains("Current tasks.md"));
    }

    #[test]
    fn a_very_long_document_is_cut_with_a_note() {
        let store = seeded();
        store.insert("specs/project.md", &"x".repeat(MAX_DOCUMENT_CHARS + 10));
        assert!(compose(&store, "0002-dark", Step::Specify)
            .contains("the rest of this document is omitted"));
    }
}

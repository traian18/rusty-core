//! Finishing a change: its requirements are merged into the capability specs they name, and the
//! folder moves to the archive. Nothing is written unless every merge is possible.
use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    layout::Layout,
    model::{Requirement, Status},
    parse::{
        self, front,
        spec::{capability_ref, Action},
    },
    store::{SpecError, SpecStore},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Archived {
    /// The folder the change now lives in, relative to the repository.
    pub archived_dir: String,
    /// The capabilities that were created or changed.
    pub merged: Vec<String>,
    /// Requirements without a `Capability:` line, which were not merged anywhere.
    pub unmerged: Vec<String>,
}

/// Merges change `dir` into the capabilities and moves it to `archive/<date>-<dir>`. Unless
/// `force`, the change must be done and every task ticked.
pub fn archive(
    store: &dyn SpecStore,
    dir: &str,
    date: &str,
    force: bool,
) -> Result<Archived, SpecError> {
    let layout = Layout::of(store);
    let base = layout.change(dir);
    if !store.dirs(&layout.changes()).iter().any(|d| d == dir) {
        return Err(SpecError::new(format!("{dir} is not a change")));
    }
    if !force {
        check_finished(store, &base)?;
    }
    let spec = store
        .read(&format!("{base}/spec.md"))
        .map(|t| parse::spec::parse(&t))
        .unwrap_or_default();
    let merges = merge_all(store, &layout, &spec.requirements, dir, date)?;
    for (path, text) in &merges.texts {
        store.write(path, text)?;
    }
    finish_change_record(store, &base, dir, date)?;
    let target = format!("{}/{date}-{dir}", layout.archive());
    store.move_dir(&base, &target)?;
    Ok(Archived {
        archived_dir: target,
        merged: merges.names,
        unmerged: merges.unmerged,
    })
}

fn check_finished(store: &dyn SpecStore, base: &str) -> Result<(), SpecError> {
    let status = store
        .read(&format!("{base}/change.md"))
        .and_then(|t| front::change(&t).status)
        .unwrap_or_default();
    let tasks = store
        .read(&format!("{base}/tasks.md"))
        .map(|t| parse::tasks::parse(&t).tasks)
        .unwrap_or_default();
    let open = tasks.iter().filter(|t| !t.done).count();
    let mut reasons = Vec::new();
    if status != Status::Done {
        reasons.push(format!("its status is {}, not done", status.word()));
    }
    if open > 0 {
        reasons.push(format!("{open} of {} tasks are not ticked", tasks.len()));
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(SpecError::new(format!(
            "The change is not finished: {}.",
            reasons.join(" and ")
        )))
    }
}

fn finish_change_record(
    store: &dyn SpecStore,
    base: &str,
    dir: &str,
    date: &str,
) -> Result<(), SpecError> {
    let path = format!("{base}/change.md");
    let mut text = store.read(&path).unwrap_or_else(|| format!("# {dir}\n"));
    text = front::set_field(&text, "status", Status::Archived.word());
    text = front::add_history(
        &text,
        &format!("{date} Archived; requirements merged into the capabilities."),
    );
    store.write(&path, &text)
}

struct Merges {
    /// New text of each capability spec, by path.
    texts: BTreeMap<String, String>,
    names: Vec<String>,
    unmerged: Vec<String>,
}

fn merge_all(
    store: &dyn SpecStore,
    layout: &Layout,
    requirements: &[Requirement],
    dir: &str,
    date: &str,
) -> Result<Merges, SpecError> {
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    let (mut names, mut unmerged, mut problems) = (Vec::new(), Vec::new(), Vec::new());
    for requirement in requirements {
        let Some(reference) = requirement.capability.as_deref().and_then(capability_ref) else {
            unmerged.push(requirement.title.clone());
            continue;
        };
        let path = layout.capability_spec(&reference.capability);
        let current = texts.get(&path).cloned().or_else(|| store.read(&path));
        let name = reference
            .requirement
            .clone()
            .unwrap_or_else(|| requirement.title.clone());
        match apply(
            current,
            &reference.capability,
            reference.action,
            &name,
            requirement,
            dir,
            date,
        ) {
            Ok(text) => {
                texts.insert(path, text);
                if !names.contains(&reference.capability) {
                    names.push(reference.capability);
                }
            }
            Err(problem) => problems.push(format!("{}: {problem}", requirement.id)),
        }
    }
    if problems.is_empty() {
        Ok(Merges {
            texts,
            names,
            unmerged,
        })
    } else {
        Err(SpecError::new(format!(
            "The requirements cannot be merged: {}",
            problems.join("; ")
        )))
    }
}

/// The capability text after the requirement is applied; `current` is `None` for a new capability.
fn apply(
    current: Option<String>,
    capability: &str,
    action: Action,
    name: &str,
    requirement: &Requirement,
    dir: &str,
    date: &str,
) -> Result<String, String> {
    let mut doc = Document::parse(&current.unwrap_or_default(), capability);
    let position = doc
        .blocks
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case(name));
    match (action, position) {
        (Action::Adds, Some(_)) => Err(format!(
            "{capability} already has a requirement named \"{name}\"; use (changes \"{name}\")"
        )),
        (Action::Adds, None) => {
            doc.blocks
                .push(Block::from_requirement(name, requirement, dir, date));
            Ok(doc.render())
        }
        (Action::Changes, Some(at)) => {
            doc.blocks[at] = Block::from_requirement(name, requirement, dir, date);
            Ok(doc.render())
        }
        (Action::Removes, Some(at)) => {
            doc.blocks.remove(at);
            Ok(doc.render())
        }
        (Action::Changes | Action::Removes, None) => Err(format!(
            "{capability} has no requirement named \"{name}\" to {}",
            if action == Action::Changes {
                "change"
            } else {
                "remove"
            }
        )),
    }
}

/// A capability spec: everything before the first requirement, then the requirement blocks.
struct Document {
    head: Vec<String>,
    blocks: Vec<Block>,
}

struct Block {
    name: String,
    lines: Vec<String>,
}

impl Block {
    fn from_requirement(name: &str, requirement: &Requirement, dir: &str, date: &str) -> Self {
        let mut lines = vec![format!("### Requirement: {name}")];
        if !requirement.detail.is_empty() {
            lines.extend(requirement.detail.lines().map(str::to_owned));
        }
        lines.push(String::new());
        lines.push("#### Scenarios".into());
        lines.extend(requirement.criteria.iter().map(|c| format!("- {}", c.text)));
        lines.push(String::new());
        lines.push(format!("_Changed by {dir} ({date})._"));
        Self {
            name: name.to_owned(),
            lines,
        }
    }
}

impl Document {
    fn parse(text: &str, capability: &str) -> Self {
        if text.trim().is_empty() {
            return Self {
                head: vec![
                    format!("# {}", title_of(capability)),
                    String::new(),
                    "Purpose: ".into(),
                    String::new(),
                    "## Requirements".into(),
                    String::new(),
                ],
                blocks: Vec::new(),
            };
        }
        let mut head = Vec::new();
        let mut blocks: Vec<Block> = Vec::new();
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("### Requirement:") {
                blocks.push(Block {
                    name: name.trim().to_owned(),
                    lines: vec![line.to_owned()],
                });
            } else if line.starts_with("## ") && !blocks.is_empty() {
                // A section after the requirements ends the last block and joins nothing: keep it with the last block's tail.
                blocks.last_mut().unwrap().lines.push(line.to_owned());
            } else if let Some(block) = blocks.last_mut() {
                block.lines.push(line.to_owned());
            } else {
                head.push(line.to_owned());
            }
        }
        Self { head, blocks }
    }

    fn render(&self) -> String {
        let mut lines: Vec<String> = self.head.clone();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        for block in &self.blocks {
            lines.push(String::new());
            let mut body = block.lines.clone();
            while body.last().is_some_and(|l| l.trim().is_empty()) {
                body.pop();
            }
            lines.extend(body);
        }
        format!("{}\n", lines.join("\n"))
    }
}

fn title_of(capability: &str) -> String {
    let mut words = capability.split('-').filter(|w| !w.is_empty()).map(|w| {
        let mut chars = w.chars();
        chars.next().map_or(String::new(), |c| {
            c.to_uppercase().collect::<String>() + chars.as_str()
        })
    });
    let first = words.next().unwrap_or_default();
    words.fold(first, |acc, w| format!("{acc} {w}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{overview, store::MemoryStore};

    const SPEC: &str = "# Albums\n\n## Requirements\n\n### R1: Albums can be created\nCapability: albums (adds \"Album creation\")\nA user creates an album.\n\n- C1: WHEN a new name is given THEN it appears.\n\n### R2: Renaming\nCapability: albums (changes \"Album naming\")\nNames stay unique.\n\n- C2: WHEN the name is taken THEN nothing changes.\n\n### R3: Old export goes\nCapability: albums (removes \"Album export\")\n\n- C3: WHEN exporting THEN it is gone.\n\n### R4: Internal\n- C4: WHEN x THEN y.\n";
    const CAPABILITY: &str = "# Albums\n\nPurpose: grouping.\n\n## Requirements\n\n### Requirement: Album naming\nOld text.\n\n#### Scenarios\n- WHEN old THEN old.\n\n### Requirement: Album export\nExports.\n\n#### Scenarios\n- WHEN x THEN y.\n";

    fn finished() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/changes/0007-albums/change.md", "---\nid: \"0007\"\ntitle: Albums\nstatus: done\n---\n# Albums\n\n## History\n- 2026-10-22 Created.\n");
        store.insert("specs/changes/0007-albums/spec.md", SPEC);
        store.insert(
            "specs/changes/0007-albums/tasks.md",
            "- [x] **T1** All. R1 (C1, C2, C3, C4)\n",
        );
        store.insert("specs/capabilities/albums/spec.md", CAPABILITY);
        store
    }

    #[test]
    fn adds_changes_and_removes_are_merged_and_the_change_moves_to_the_archive() {
        let store = finished();
        let done = archive(&store, "0007-albums", "2026-10-30", false).unwrap();
        assert_eq!(
            (done.merged.as_slice(), done.unmerged.as_slice()),
            (&["albums".to_owned()][..], &["Internal".to_owned()][..])
        );
        assert_eq!(done.archived_dir, "specs/archive/2026-10-30-0007-albums");
        let text = store.read("specs/capabilities/albums/spec.md").unwrap();
        assert!(text.starts_with("# Albums\n\nPurpose: grouping.\n\n## Requirements\n"));
        assert!(text.contains("### Requirement: Album naming\nNames stay unique.\n\n#### Scenarios\n- WHEN the name is taken THEN nothing changes.\n\n_Changed by 0007-albums (2026-10-30)._"));
        assert!(!text.contains("Old text.") && !text.contains("Album export"));
        assert!(text.contains("### Requirement: Album creation\nA user creates an album."));
        assert!(text.ends_with("_Changed by 0007-albums (2026-10-30)._\n"));
        assert!(store.dirs("specs/changes").is_empty());
        let record = store
            .read("specs/archive/2026-10-30-0007-albums/change.md")
            .unwrap();
        assert!(
            record.contains("status: archived") && record.contains("Archived; requirements merged")
        );
        let seen = overview(&store);
        assert_eq!(seen.capabilities[0].requirements, 2);
        assert_eq!(seen.archive[0].title, "Albums");
    }

    #[test]
    fn a_new_capability_is_created_from_adds() {
        let store = finished();
        store.insert("specs/changes/0007-albums/spec.md", "# A\n\n## Requirements\n### R1: Dark\nCapability: theme-switching (adds \"Dark mode\")\n- C1: WHEN on THEN dark.\n");
        archive(&store, "0007-albums", "2026-10-30", false).unwrap();
        let text = store
            .read("specs/capabilities/theme-switching/spec.md")
            .unwrap();
        assert!(text.starts_with(
            "# Theme Switching\n\nPurpose: \n\n## Requirements\n\n### Requirement: Dark mode\n"
        ));
    }

    #[test]
    fn nothing_is_written_when_one_merge_is_impossible() {
        let store = finished();
        store.insert("specs/changes/0007-albums/spec.md", "# A\n\n## Requirements\n### R1: A\nCapability: albums (adds \"Album naming\")\n- C1: WHEN a THEN b.\n### R2: B\nCapability: albums (changes \"Missing\")\n- C2: WHEN a THEN b.\n");
        let error = archive(&store, "0007-albums", "2026-10-30", false)
            .unwrap_err()
            .message;
        assert!(
            error.contains("R1: albums already has a requirement named \"Album naming\"")
                && error.contains("R2: albums has no requirement named \"Missing\" to change")
        );
        assert_eq!(
            store.read("specs/capabilities/albums/spec.md").unwrap(),
            CAPABILITY
        );
        assert_eq!(store.dirs("specs/changes"), ["0007-albums"]);
    }

    #[test]
    fn an_unfinished_change_needs_force() {
        let store = finished();
        store.insert(
            "specs/changes/0007-albums/change.md",
            "---\nstatus: building\n---\n# A\n",
        );
        store.insert(
            "specs/changes/0007-albums/tasks.md",
            "- [x] **T1** a. R1\n- [ ] **T2** b. R1\n",
        );
        let error = archive(&store, "0007-albums", "2026-10-30", false)
            .unwrap_err()
            .message;
        assert!(
            error.contains("its status is building, not done")
                && error.contains("1 of 2 tasks are not ticked")
        );
        assert!(archive(&store, "0007-albums", "2026-10-30", true).is_ok());
    }

    #[test]
    fn an_unknown_change_is_refused() {
        assert!(archive(&MemoryStore::default(), "0001-x", "d", false).is_err());
    }

    #[test]
    fn two_requirements_for_one_capability_are_applied_in_order() {
        let store = finished();
        store.insert("specs/changes/0007-albums/spec.md", "# A\n\n## Requirements\n### R1: A\nCapability: albums (adds \"One\")\n- C1: WHEN a THEN b.\n### R2: B\nCapability: albums (changes \"One\")\n- C2: WHEN c THEN d.\n");
        archive(&store, "0007-albums", "2026-10-30", false).unwrap();
        let text = store.read("specs/capabilities/albums/spec.md").unwrap();
        assert_eq!(text.matches("### Requirement: One").count(), 1);
        assert!(text.contains("WHEN c THEN d."));
    }
}

//! Onboarding an existing project: a survey proposes the capabilities as a list the person edits
//! (`onboarding.md`), then each capability's spec is written from the code it names, one run each.
use serde::{Deserialize, Serialize};

use crate::{
    layout::{slugify, Layout},
    store::SpecStore,
};

/// The file name of the capability list, inside the specs root.
pub const FILE: &str = "onboarding.md";

/// One capability the survey proposed.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The folder name under `capabilities/`.
    pub name: String,
    pub title: String,
    pub purpose: String,
    /// Files and folders to start reading from.
    pub paths: Vec<String>,
    pub notes: String,
    /// Whether `capabilities/<name>/spec.md` exists.
    pub written: bool,
}

pub fn path(layout: &Layout) -> String {
    format!("{}/{FILE}", layout.root)
}

/// The capabilities listed in `onboarding.md`: each is a `## name` section with `Purpose:`,
/// `Paths:` (comma separated, backticks allowed) and `Notes:` lines. Text before the first section
/// is instructions and ignored; a section without a usable name is skipped.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let title = heading.trim().trim_matches('`').to_owned();
            entries.push(Entry {
                name: slugify(&title),
                title,
                ..Default::default()
            });
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Purpose:") {
            entry.purpose = rest.trim().to_owned();
        } else if let Some(rest) = trimmed.strip_prefix("Paths:") {
            entry.paths = rest
                .split(',')
                .map(|p| p.trim().trim_matches('`').trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect();
        } else if let Some(rest) = trimmed.strip_prefix("Notes:") {
            entry.notes = rest.trim().to_owned();
        } else if !trimmed.is_empty() && !entry.notes.is_empty() {
            entry.notes.push('\n');
            entry.notes.push_str(trimmed);
        }
    }
    entries.retain(|entry| !entry.title.is_empty());
    entries.dedup_by(|a, b| a.name == b.name);
    entries
}

/// The listed capabilities with whether each has a spec yet; `None` when there is no list.
pub fn read(store: &dyn SpecStore, layout: &Layout) -> Option<Vec<Entry>> {
    let text = store.read(&path(layout))?;
    let mut entries = parse(&text);
    for entry in &mut entries {
        entry.written = store.read(&layout.capability_spec(&entry.name)).is_some();
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    const LIST: &str = "# Onboarding\n\nEdit this list.\n\n## Albums\nPurpose: grouping photos.\nPaths: `src/albums/`, src/ui/AlbumSidebar.tsx\nNotes: renaming is the tricky part\nand names are unique.\n\n## photo-import\nPurpose: bringing photos in\n\n## \n";

    #[test]
    fn sections_become_entries_with_purpose_paths_and_notes() {
        let entries = parse(LIST);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[0].name.as_str(), entries[0].title.as_str()),
            ("albums", "Albums")
        );
        assert_eq!(entries[0].purpose, "grouping photos.");
        assert_eq!(entries[0].paths, ["src/albums/", "src/ui/AlbumSidebar.tsx"]);
        assert_eq!(
            entries[0].notes,
            "renaming is the tricky part\nand names are unique."
        );
        assert_eq!(entries[1].name, "photo-import");
        assert!(entries[1].paths.is_empty());
    }

    #[test]
    fn a_repeated_name_is_listed_once() {
        assert_eq!(parse("## A\nPurpose: x\n## a\nPurpose: y\n").len(), 1);
    }

    #[test]
    fn it_says_which_capabilities_are_written() {
        let store = MemoryStore::default();
        let layout = Layout::new("specs");
        assert!(read(&store, &layout).is_none());
        store.insert("specs/onboarding.md", LIST);
        store.insert("specs/capabilities/albums/spec.md", "# Albums\n");
        let entries = read(&store, &layout).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.written).collect::<Vec<_>>(),
            [true, false]
        );
    }
}

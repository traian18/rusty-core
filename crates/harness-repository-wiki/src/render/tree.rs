//! The tree the Docs tab walks: repository, packages and folders, then files.
use harness_workspace::{
    dependency_groups::GroupKind,
    folder_tree::{self, REST_PREFIX},
};
use serde::Serialize;

use super::Cx;
use crate::{
    facts::path_text,
    model::{PageId, PageLevel},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TreeEntry {
    pub id: PageId,
    pub level: PageLevel,
    pub title: String,
    pub files: usize,
    pub has_children: bool,
}

fn group_entry(
    group: &harness_workspace::dependency_groups::Group,
    has_children: bool,
) -> TreeEntry {
    let level = match group.kind {
        GroupKind::DeclaredPackage | GroupKind::SourceRoot => PageLevel::Package,
        _ => PageLevel::Folder,
    };
    TreeEntry {
        id: PageId::new(level, &group.id.0),
        level,
        title: group.label.clone(),
        files: group.members.len(),
        has_children,
    }
}

pub(super) fn children(cx: &mut Cx<'_>, level: PageLevel, target: &str) -> Vec<TreeEntry> {
    match level {
        PageLevel::File => Vec::new(),
        PageLevel::Repository => {
            let (_, top) = folder_tree::top_level(cx.snapshot);
            top.iter()
                .map(|group| group_entry(group, !group.members.is_empty()))
                .collect()
        }
        PageLevel::Package | PageLevel::Folder => {
            let Some(group) = folder_tree::group_of(cx.snapshot, target) else {
                return Vec::new();
            };
            let listing = folder_tree::list(&group);
            let mut entries: Vec<TreeEntry> = listing
                .folders
                .iter()
                .map(|f| group_entry(&f.group, true))
                .collect();
            entries.sort_by(|a, b| b.files.cmp(&a.files).then(a.id.cmp(&b.id)));
            if listing.is_leaf() {
                entries.extend(listing.loose.iter().map(|file| TreeEntry {
                    id: PageId::file(&path_text(file)),
                    level: PageLevel::File,
                    title: path_text(file),
                    files: 1,
                    has_children: false,
                }));
            } else if !listing.loose.is_empty() {
                entries.push(TreeEntry {
                    id: PageId::folder(&format!("{REST_PREFIX}{}", group.id.0)),
                    level: PageLevel::Folder,
                    title: "files".to_owned(),
                    files: listing.loose.len(),
                    has_children: true,
                });
            }
            entries
        }
    }
}

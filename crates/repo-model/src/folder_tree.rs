//! The repository as a folder tree, one level at a time.
//!
//! Every consumer that shows structure (the architecture canvas, `repo_context`) reads the same
//! rule: the direct sub-folders of a focus are one entry each, and the files that sit in none of
//! them are one more entry. A focus with no sub-folders is a leaf, and its files are what to show.
//! Folders are derived from indexed file paths; they are not boundaries a manifest declares, and
//! their groups say so (`GroupBasis::Inferred`).
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    dependency_groups::{Group, GroupBasis, GroupId, GroupKind},
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};

/// Prefix of a group id that stands for a folder derived from file paths.
pub const DIRECTORY_PREFIX: &str = "dir:";
/// Prefix of a group id that stands for the files of a focus that sit in none of its folders.
pub const REST_PREFIX: &str = "rest:";

/// A folder of files, derived from paths.
pub fn directory_group(path: &Path, label: String, members: Vec<RootRelativePath>) -> Group {
    let shown = path.display();
    Group {
        id: GroupId(format!("{DIRECTORY_PREFIX}{shown}")),
        kind: GroupKind::Directory,
        label,
        basis: GroupBasis::Inferred,
        explanation: format!(
            "Inferred: the files under `{shown}`. Directories inside a package are read from file \
             paths; they are not boundaries declared by a manifest."
        ),
        evidence: Vec::new(),
        cycle_sites: Vec::new(),
        evidence_truncated: false,
        involves_type_only: false,
        members,
    }
}

/// The folder path a `dir:` group names.
pub fn directory_path(group: &Group) -> Option<&str> {
    group.id.0.strip_prefix(DIRECTORY_PREFIX)
}

/// The folder that holds every member of the group.
pub fn common_directory(group: &Group) -> PathBuf {
    let mut common: Option<Vec<_>> = None;
    for file in &group.members {
        let dir = file
            .as_path()
            .parent()
            .map(|p| p.components().collect::<Vec<_>>())
            .unwrap_or_default();
        common = Some(match common {
            None => dir,
            Some(current) => current
                .into_iter()
                .zip(dir)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    common.unwrap_or_default().into_iter().collect()
}

/// The folder a group is read from: the folder a `dir:` group names, otherwise the deepest folder
/// that holds all of its files.
pub fn base_directory(group: &Group) -> PathBuf {
    directory_path(group).map_or_else(|| common_directory(group), PathBuf::from)
}

/// The folders directly inside the group's folder, each with every file under it. Files that sit
/// in the folder itself belong to none of them. One level only: opening a folder shows the next.
pub fn immediate_directories(focus: &Group) -> Vec<Group> {
    let base = base_directory(focus);
    let mut buckets: BTreeMap<PathBuf, Vec<RootRelativePath>> = BTreeMap::new();
    for file in &focus.members {
        let Ok(below) = file.as_path().strip_prefix(&base) else {
            continue;
        };
        let mut parts = below.components();
        if let (Some(first), Some(_file_name)) = (parts.next(), parts.next()) {
            buckets
                .entry(base.join(first.as_os_str()))
                .or_default()
                .push(file.clone());
        }
    }
    buckets
        .into_iter()
        .map(|(dir, mut members)| {
            members.sort();
            let label = dir
                .strip_prefix(&base)
                .unwrap_or(&dir)
                .display()
                .to_string();
            directory_group(&dir, label, members)
        })
        .collect()
}

/// Folders strictly below `base` that hold files, each with all files under it. With `top_only`
/// only the first level below `base` is produced.
pub fn directories_below<'a>(
    files: impl Iterator<Item = &'a RootRelativePath>,
    base: &Path,
    top_only: bool,
) -> Vec<Group> {
    let mut buckets: BTreeMap<PathBuf, Vec<RootRelativePath>> = BTreeMap::new();
    let base_depth = base.components().count();
    for file in files {
        let directories = file
            .as_path()
            .ancestors()
            .skip(1)
            .filter(|dir| dir.starts_with(base) && dir.components().count() > base_depth)
            .collect::<Vec<_>>();
        let chosen = if top_only {
            directories.last().copied().into_iter().collect()
        } else {
            directories
        };
        for dir in chosen {
            buckets
                .entry(dir.to_path_buf())
                .or_default()
                .push(file.clone());
        }
    }
    buckets
        .into_iter()
        .map(|(dir, mut members)| {
            members.sort();
            let label = dir.strip_prefix(base).unwrap_or(&dir).display().to_string();
            directory_group(&dir, label, members)
        })
        .collect()
}

/// One sub-folder of a focus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Folder {
    pub group: Group,
    /// True when some file sits deeper than the folder itself, so opening it shows more folders.
    pub has_folders: bool,
}

/// What one level of the tree shows for a focus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderListing {
    /// The direct sub-folders, in path order.
    pub folders: Vec<Folder>,
    /// The files that sit in none of the sub-folders, sorted.
    pub loose: Vec<RootRelativePath>,
}

impl FolderListing {
    /// A focus with no sub-folders: its files are the thing to show.
    pub fn is_leaf(&self) -> bool {
        self.folders.is_empty()
    }
}

/// The sub-folders and loose files of a focus.
pub fn list(focus: &Group) -> FolderListing {
    let folders = immediate_directories(focus);
    let loose = focus
        .members
        .iter()
        .filter(|file| !folders.iter().any(|folder| folder.contains(file)))
        .cloned()
        .collect();
    FolderListing {
        folders: folders
            .into_iter()
            .map(|group| {
                let has_folders = directory_path(&group).is_some_and(|path| {
                    group
                        .members
                        .iter()
                        .any(|file| file.as_path().parent() != Some(Path::new(path)))
                });
                Folder { group, has_folders }
            })
            .collect(),
        loose,
    }
}

/// The folder `dir:<path>` as a group, if any indexed file lives under it. `dir:` alone is the
/// whole repository.
fn directory_group_of(snapshot: &RepositorySnapshot, id: &str) -> Option<Group> {
    let path = PathBuf::from(id.strip_prefix(DIRECTORY_PREFIX)?);
    let mut members = snapshot
        .imports
        .files
        .iter()
        .filter(|file| file.path.as_path().starts_with(&path))
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    if members.is_empty() {
        return None;
    }
    members.sort();
    let label = path.display().to_string();
    let label = if label.is_empty() {
        ".".to_owned()
    } else {
        label
    };
    Some(directory_group(&path, label, members))
}

/// The files of `rest:<focus>`: the focus's files that are in none of its direct sub-folders.
fn rest_group(snapshot: &RepositorySnapshot, id: &str) -> Option<Group> {
    let focus_id = id.strip_prefix(REST_PREFIX)?;
    let focus = group_of(snapshot, focus_id)?;
    let members = list(&focus).loose;
    (!members.is_empty()).then(|| Group {
        id: GroupId(id.to_owned()),
        label: format!("Files in {}", focus.label),
        explanation: format!(
            "Inferred: the files of `{}` that are in none of its folders.",
            focus.label
        ),
        members,
        ..focus
    })
}

/// A declared group, or a group that exists only as a view (`dir:…`, `rest:…`).
pub fn group_of(snapshot: &RepositorySnapshot, id: &str) -> Option<Group> {
    snapshot
        .groups
        .group(&GroupId(id.to_owned()))
        .cloned()
        .or_else(|| derived_group(snapshot, id))
}

/// A group that exists only as a view: a `dir:` folder or the loose files of a focus.
pub fn derived_group(snapshot: &RepositorySnapshot, id: &str) -> Option<Group> {
    if id.starts_with(REST_PREFIX) {
        rest_group(snapshot, id)
    } else {
        directory_group_of(snapshot, id)
    }
}

/// The kind of group that stands for "a package" in this repository, if any are declared.
pub fn declared_unit(snapshot: &RepositorySnapshot) -> Option<GroupKind> {
    [GroupKind::DeclaredPackage, GroupKind::SourceRoot]
        .into_iter()
        .find(|kind| {
            snapshot
                .groups
                .groups
                .iter()
                .any(|group| group.kind == *kind)
        })
}

/// The top of the tree: the declared packages, or the top-level folders when there are none.
pub fn top_level(snapshot: &RepositorySnapshot) -> (GroupKind, Vec<Group>) {
    match declared_unit(snapshot) {
        Some(kind) => (
            kind,
            snapshot
                .groups
                .groups
                .iter()
                .filter(|group| group.kind == kind)
                .cloned()
                .collect(),
        ),
        None => (
            GroupKind::Directory,
            directories_below(
                snapshot.imports.files.iter().map(|file| &file.path),
                Path::new(""),
                true,
            ),
        ),
    }
}

#[cfg(test)]
mod tests;

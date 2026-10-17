//! The file changes one non-merge commit made relative to its parent.
use std::path::Path;

use git2::{Commit, Delta, Diff, DiffFindOptions, DiffOptions, Patch, Repository};
use harness_workspace::repository_intelligence::RootRelativePath;

use crate::{
    error::GitAnalysisError,
    model::{ChangeKind, FileChange},
};

pub(super) fn changes(
    repo: &Repository,
    commit: &Commit<'_>,
    prefix: &Path,
    max_line_stat_files: usize,
) -> Result<Vec<FileChange>, GitAnalysisError> {
    let failed = |error: git2::Error| GitAnalysisError::Failed(error.message().to_owned());
    let tree = commit.tree().map_err(failed)?;
    let parent_tree = match commit.parents().next() {
        Some(parent) => Some(parent.tree().map_err(failed)?),
        None => None,
    };
    let mut options = DiffOptions::new();
    options.ignore_submodules(true);
    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
        .map_err(failed)?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))
        .map_err(failed)?;
    let with_lines = diff.deltas().len() <= max_line_stat_files;
    let mut changes = Vec::new();
    for (index, delta) in diff.deltas().enumerate() {
        let Some(kind) = change_kind(delta.status()) else {
            continue;
        };
        let new = delta.new_file().path();
        let old = delta.old_file().path();
        let current = if kind == ChangeKind::Deleted {
            old
        } else {
            new
        };
        let Some(path) = current.and_then(|p| inside_root(p, prefix)) else {
            continue;
        };
        let previous_path = (kind == ChangeKind::Renamed)
            .then(|| old.and_then(|p| inside_root(p, prefix)))
            .flatten();
        let kind = if kind == ChangeKind::Renamed && previous_path.is_none() {
            ChangeKind::Added
        } else {
            kind
        };
        let (lines_added, lines_removed) = if with_lines {
            line_counts(&diff, index)
        } else {
            (None, None)
        };
        changes.push(FileChange {
            path,
            previous_path,
            kind,
            lines_added,
            lines_removed,
        });
    }
    Ok(changes)
}

fn change_kind(status: Delta) -> Option<ChangeKind> {
    match status {
        Delta::Added | Delta::Copied => Some(ChangeKind::Added),
        Delta::Deleted => Some(ChangeKind::Deleted),
        Delta::Modified | Delta::Typechange => Some(ChangeKind::Modified),
        Delta::Renamed => Some(ChangeKind::Renamed),
        _ => None,
    }
}

/// `path` relative to the analysed root, or `None` when it lies outside it.
fn inside_root(path: &Path, prefix: &Path) -> Option<RootRelativePath> {
    RootRelativePath::new(path.strip_prefix(prefix).ok()?).ok()
}

/// Line counts for one delta; `(None, None)` for binary files, which only a loaded patch can reveal.
fn line_counts(diff: &Diff<'_>, index: usize) -> (Option<u32>, Option<u32>) {
    let Ok(Some(patch)) = Patch::from_diff(diff, index) else {
        return (None, None);
    };
    if patch.delta().flags().is_binary() {
        return (None, None);
    }
    match patch.line_stats() {
        Ok((_, added, removed)) => (Some(added as u32), Some(removed as u32)),
        Err(_) => (None, None),
    }
}

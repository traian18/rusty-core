//! Removing the candidates a report listed.

use std::fs;
use std::path::Path;

use super::{Candidate, Kind, Report};

/// What [`apply`] removed and what it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removed {
    pub items: usize,
    pub bytes: u64,
    /// Candidates that could not be removed, with the reason.
    pub failed: Vec<(Candidate, String)>,
}

/// Removes every candidate of `report`. The caller shows the report and gets the user's
/// confirmation first; nothing outside the report is touched.
pub fn apply(root: &Path, report: &Report) -> Removed {
    let mut removed = Removed::default();
    for candidate in &report.candidates {
        match remove(candidate) {
            Ok(()) => {
                removed.items += 1;
                removed.bytes += candidate.bytes;
            }
            Err(error) => removed.failed.push((candidate.clone(), error.to_string())),
        }
    }
    if report.candidates.iter().any(|c| c.kind == Kind::Worktree) {
        forget_missing_worktrees(root);
    }
    removed
}

fn remove(candidate: &Candidate) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(&candidate.path)?;
    if meta.is_dir() {
        fs::remove_dir_all(&candidate.path)
    } else {
        fs::remove_file(&candidate.path)
    }
}

/// Git still lists a worktree whose folder is gone until it is pruned.
fn forget_missing_worktrees(root: &Path) {
    let Ok(repo) = git2::Repository::open(root) else {
        return;
    };
    let Ok(names) = repo.worktrees() else {
        return;
    };
    for name in names.iter().flatten().flatten() {
        if let Ok(worktree) = repo.find_worktree(name) {
            if !worktree.path().exists() {
                let _ = worktree.prune(None);
            }
        }
    }
}

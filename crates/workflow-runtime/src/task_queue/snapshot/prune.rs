//! Removing the checkpoints of old runs.
//!
//! Every task of every run leaves refs under `refs/rusty/checkpoints/<run>/…`. They are what makes
//! an undo possible, but after many runs they pile up: thousands of refs slow down `git status`,
//! `git branch` and fetches, and keep every checkpointed tree alive. Pruning deletes the refs of
//! runs older than a given age (the commits become ordinary unreachable objects that git's own
//! garbage collection removes). The refs of the run that is going on are never touched.

use std::path::Path;

use git2::Repository;

use super::repositories;

/// Where checkpoint refs live.
const PREFIX: &str = "refs/rusty/checkpoints/";

/// How long a run's checkpoints are kept by default.
pub const DEFAULT_MAX_AGE_DAYS: u64 = 14;

/// What a prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Checkpoint refs deleted.
    pub refs: usize,
    /// Repositories that had some.
    pub repositories: usize,
}

/// Deletes the checkpoint refs older than `max_age_days` in every repository under `root`, except
/// those of `keep_run`. Best effort: a repository that cannot be read is skipped.
pub fn prune_checkpoints(root: &Path, max_age_days: u64, keep_run: Option<&str>) -> Pruned {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    prune_older_than(root, now - (max_age_days as i64) * 86_400, keep_run)
}

/// Deletes the checkpoint refs whose commit is older than `oldest` (seconds since the epoch).
pub(super) fn prune_older_than(root: &Path, oldest: i64, keep_run: Option<&str>) -> Pruned {
    let mut total = Pruned::default();
    for (_, path) in repositories(root) {
        let removed = prune_repository(&path, oldest, keep_run);
        total.refs += removed;
        total.repositories += usize::from(removed > 0);
    }
    total
}

fn prune_repository(path: &Path, oldest: i64, keep_run: Option<&str>) -> usize {
    let Ok(repo) = Repository::open(path) else {
        return 0;
    };
    let old: Vec<String> = checkpoint_refs(&repo)
        .into_iter()
        .filter(|(name, _)| !is_kept(name, keep_run))
        .filter(|(_, time)| *time < oldest)
        .map(|(name, _)| name)
        .collect();
    old.iter()
        .filter(|name| {
            repo.find_reference(name)
                .and_then(|mut reference| reference.delete())
                .is_ok()
        })
        .count()
}

/// Every checkpoint ref with the time its commit was made.
fn checkpoint_refs(repo: &Repository) -> Vec<(String, i64)> {
    let Ok(references) = repo.references() else {
        return Vec::new();
    };
    references
        .flatten()
        .filter_map(|reference| {
            let name = reference.name().ok()?.to_owned();
            if !name.starts_with(PREFIX) {
                return None;
            }
            let time = reference.peel_to_commit().ok()?.time().seconds();
            Some((name, time))
        })
        .collect()
}

/// The run being worked on keeps its checkpoints whatever their age.
fn is_kept(name: &str, keep_run: Option<&str>) -> bool {
    keep_run.is_some_and(|run| {
        let clean: String = run
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        name.starts_with(&format!("{PREFIX}{}/", clean.trim_matches('/')))
    })
}

//! Looking for leftovers, without changing anything.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{Candidate, Kind, Policy, Report};

const SESSIONS_DIR: &str = ".harness/sessions";
const WORKTREES_DIR: &str = ".harness-worktrees";

/// The sessions, run stores and worktrees under `root` that nothing has touched for as long as the
/// policy says. Oldest first.
pub fn scan(root: &Path, policy: &Policy) -> Report {
    scan_at(root, policy, SystemTime::now())
}

pub(super) fn scan_at(root: &Path, policy: &Policy, now: SystemTime) -> Report {
    let mut candidates = Vec::new();
    collect(
        &root.join(SESSIONS_DIR),
        Kind::Session,
        policy.sessions,
        now,
        &mut candidates,
    );
    collect(
        &root.join(WORKTREES_DIR),
        Kind::Worktree,
        policy.worktrees,
        now,
        &mut candidates,
    );
    for store in &policy.run_store_roots {
        collect(
            store,
            Kind::RunStore,
            policy.run_stores,
            now,
            &mut candidates,
        );
    }
    candidates.sort_by(|a, b| b.idle_days.cmp(&a.idle_days).then(a.path.cmp(&b.path)));
    Report { candidates }
}

/// Every entry of `folder` that has been idle for `min_idle`.
fn collect(
    folder: &Path,
    kind: Kind,
    min_idle: Duration,
    now: SystemTime,
    out: &mut Vec<Candidate>,
) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(changed) = last_change(&path) else {
            continue;
        };
        let idle = now.duration_since(changed).unwrap_or_default();
        if idle >= min_idle {
            out.push(Candidate {
                kind,
                bytes: size_of(&path),
                idle_days: idle.as_secs() / 86_400,
                path,
            });
        }
    }
}

/// The newest modification time of the entry and, for a folder, of everything inside it.
fn last_change(path: &Path) -> Option<SystemTime> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            if let Some(inner) = last_change(&entry.path()) {
                newest = newest.max(inner);
            }
        }
    }
    Some(newest)
}

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

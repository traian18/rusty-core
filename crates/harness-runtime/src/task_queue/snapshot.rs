//! Snapshots of the workspace's git repositories, so a task that failed can
//! start again from the tree it started from instead of from its own
//! half-finished edits.
//!
//! A snapshot records the working tree of every repository (the workspace
//! root when it is one, otherwise each folder directly inside it that is) as
//! a commit under `refs/rusty/checkpoints/…`. It never moves a branch, never
//! changes the index and never touches ignored files. Undoing an attempt
//! first snapshots the attempt itself, so nothing is lost: the attempt's
//! changes stay reachable under their own ref.

use std::path::{Path, PathBuf};

use git2::{Delta, DiffOptions, IndexAddOption, Oid, Repository, RepositoryOpenFlags, Signature};
use serde::{Deserialize, Serialize};

/// One repository's recorded tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoSnapshot {
    /// The repository folder relative to the workspace root ("" for the root).
    pub repo: String,
    pub commit: String,
}

/// The recorded trees of every repository in the workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub label: String,
    pub repos: Vec<RepoSnapshot>,
}

/// What undoing an attempt did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Undone {
    /// Files put back as they were (`repo/path`).
    pub files: Vec<String>,
    /// Where the undone attempt is kept.
    pub attempt: Snapshot,
}

/// Records every repository under `root`. Repositories that cannot be read
/// are left out; no repository at all gives an empty snapshot.
pub fn take(root: &Path, label: &str) -> Snapshot {
    let repos = repositories(root)
        .into_iter()
        .filter_map(|(name, path)| {
            let commit = record(&path, label).ok()?;
            Some(RepoSnapshot {
                repo: name,
                commit: commit.to_string(),
            })
        })
        .collect();
    Snapshot {
        label: label.to_owned(),
        repos,
    }
}

/// Puts every file that changed since `baseline` back as it was, after
/// recording the current state under `<label>`.
pub fn undo_to(root: &Path, baseline: &Snapshot, label: &str) -> Result<Undone, String> {
    let attempt = take(root, label);
    let mut files = Vec::new();
    for base in &baseline.repos {
        let Some(now) = attempt.repos.iter().find(|repo| repo.repo == base.repo) else {
            continue;
        };
        let path = if base.repo.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&base.repo)
        };
        for file in
            restore(&path, &base.commit, &now.commit).map_err(|e| format!("{}: {e}", base.repo))?
        {
            files.push(if base.repo.is_empty() {
                file
            } else {
                format!("{}/{file}", base.repo)
            });
        }
    }
    Ok(Undone { files, attempt })
}

/// The workspace root when it is a repository, otherwise the repositories
/// directly inside it, as (name relative to the root, path).
fn repositories(root: &Path) -> Vec<(String, PathBuf)> {
    if is_repository(root) {
        return vec![(String::new(), root.to_path_buf())];
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && is_repository(path))
        .filter_map(|path| Some((path.file_name()?.to_str()?.to_owned(), path)))
        .collect();
    found.sort();
    found
}

fn is_repository(path: &Path) -> bool {
    path.join(".git").exists()
        && Repository::open_ext(
            path,
            RepositoryOpenFlags::NO_SEARCH,
            std::iter::empty::<&Path>(),
        )
        .is_ok()
}

/// The tree of every repository's working tree, as (repository, tree id),
/// without recording anything: two equal fingerprints mean no file changed.
pub fn fingerprint(root: &Path) -> Vec<(String, String)> {
    repositories(root)
        .into_iter()
        .filter_map(|(name, path)| {
            let repo = Repository::open(&path).ok()?;
            Some((name, working_tree(&repo).ok()?.to_string()))
        })
        .collect()
}

/// The changes from `base` to `now` as a unified diff, cut to about
/// `max_chars` (a planner reads it to reuse what an undone attempt learned).
pub fn patch(root: &Path, base: &Snapshot, now: &Snapshot, max_chars: usize) -> String {
    let mut text = String::new();
    for base_repo in &base.repos {
        let Some(now_repo) = now.repos.iter().find(|repo| repo.repo == base_repo.repo) else {
            continue;
        };
        let path = root.join(&base_repo.repo);
        let Ok(diff) = repo_patch(&path, &base_repo.commit, &now_repo.commit) else {
            continue;
        };
        if diff.is_empty() {
            continue;
        }
        if !base_repo.repo.is_empty() {
            text.push_str(&format!("# repository {}\n", base_repo.repo));
        }
        text.push_str(&diff);
        if text.len() > max_chars {
            let mut cut = max_chars;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n… (diff cut; the whole attempt is kept under its ref)");
            break;
        }
    }
    text
}

fn repo_patch(path: &Path, base: &str, now: &str) -> Result<String, git2::Error> {
    let repo = Repository::open(path)?;
    let base_tree = repo.find_commit(Oid::from_str(base)?)?.tree()?;
    let now_tree = repo.find_commit(Oid::from_str(now)?)?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&now_tree), None)?;
    let mut text = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            text.push(line.origin());
        }
        text.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(text)
}

/// The working tree (tracked and untracked, not ignored) as a tree object.
/// The index is changed in memory only and never written back.
fn working_tree(repo: &Repository) -> Result<Oid, git2::Error> {
    let mut index = repo.index()?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
    index.update_all(["*"], None)?;
    index.write_tree_to(repo)
}

/// Commits the working tree (tracked and untracked, not ignored) without
/// touching the index on disk, and points `refs/rusty/checkpoints/<label>` at it.
fn record(path: &Path, label: &str) -> Result<Oid, git2::Error> {
    let repo = Repository::open(path)?;
    let tree = repo.find_tree(working_tree(&repo)?)?;
    let signature = repo
        .signature()
        .or_else(|_| Signature::now("Rusty", "rusty@localhost"))?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let commit = repo.commit(
        None,
        &signature,
        &signature,
        &format!("rusty checkpoint {label}"),
        &tree,
        &parents,
    )?;
    repo.reference(&ref_name(label), commit, true, "rusty checkpoint")?;
    Ok(commit)
}

/// Git ref names may not contain spaces and a few other characters.
fn ref_name(label: &str) -> String {
    let clean: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("refs/rusty/checkpoints/{}", clean.trim_matches('/'))
}

/// Writes every file that differs between the two commits back to its
/// `base` content, deleting files `base` did not have. Returns their paths.
fn restore(path: &Path, base: &str, now: &str) -> Result<Vec<String>, git2::Error> {
    let repo = Repository::open(path)?;
    let base_tree = repo.find_commit(Oid::from_str(base)?)?.tree()?;
    let now_tree = repo.find_commit(Oid::from_str(now)?)?.tree()?;
    let diff = repo.diff_tree_to_tree(
        Some(&base_tree),
        Some(&now_tree),
        Some(&mut DiffOptions::new()),
    )?;
    let mut changed = Vec::new();
    for delta in diff.deltas() {
        let Some(file) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
            continue;
        };
        let target = path.join(file);
        if delta.status() == Delta::Added {
            let _ = std::fs::remove_file(&target);
        } else {
            let entry = base_tree.get_path(file)?;
            let blob = repo.find_blob(entry.id())?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io_error)?;
            }
            std::fs::write(&target, blob.content()).map_err(io_error)?;
            if entry.filemode() == 0o100755 {
                set_executable(&target);
            }
        }
        changed.push(file.to_string_lossy().into_owned());
    }
    Ok(changed)
}

fn io_error(error: std::io::Error) -> git2::Error {
    git2::Error::from_str(&error.to_string())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut permissions = meta.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[cfg(test)]
mod tests;

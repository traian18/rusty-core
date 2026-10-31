//! The Git repositories in a workspace folder, at any depth: the folder itself when it is one,
//! repositories grouped in plain folders (`services/api`, `apps/web`), and repositories inside
//! other repositories. Symlinks are not followed, and dependency, build and cache folders are
//! never searched.
use std::path::{Path, PathBuf};

use git2::{Repository, RepositoryOpenFlags};

/// Never searched for repositories: Git internals, dependencies, build output and caches.
pub const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    ".cache",
    ".next",
    "__pycache__",
    ".rusty",
];

/// Folder levels below the workspace searched at most.
pub const MAX_DEPTH: usize = 8;
/// Repositories reported at most.
pub const MAX_REPOSITORIES: usize = 256;

/// One repository found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedRepository {
    /// Relative to the workspace folder, `/`-separated; empty for the folder itself.
    pub relative: String,
    pub path: PathBuf,
}

/// Whether a folder named `name` is never searched for repositories.
pub fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name)
}

/// Whether `path` is the root of a repository's working tree (a `.git` folder, or the `.git`
/// file of a linked worktree or submodule), not merely inside one.
pub fn is_repository_root(path: &Path) -> bool {
    path.join(".git").exists()
        && Repository::open_ext(
            path,
            RepositoryOpenFlags::NO_SEARCH,
            std::iter::empty::<&Path>(),
        )
        .is_ok()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Every repository under `root`, the folder itself first, then by relative path.
pub fn nested_repositories(root: &Path) -> Vec<NestedRepository> {
    let mut found = Vec::new();
    let mut folders = vec![(root.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = folders.pop() {
        if found.len() >= MAX_REPOSITORIES {
            break;
        }
        if is_repository_root(&folder) {
            found.push(NestedRepository {
                relative: relative(root, &folder),
                path: folder.clone(),
            });
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name();
            if is_dir && !name.to_str().is_some_and(is_skipped_directory) {
                folders.push((entry.path(), depth + 1));
            }
        }
    }
    found.sort_by(|a, b| a.relative.cmp(&b.relative));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        Repository::init(path).unwrap();
    }

    #[test]
    fn repositories_are_found_in_groups_inside_repositories_and_not_in_skipped_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init(root);
        init(&root.join("rusty-core"));
        init(&root.join("services/payments/api"));
        init(&root.join("services/web"));
        init(&root.join("rusty-core/vendor/lib"));
        init(&root.join("app/node_modules/pkg"));
        std::fs::create_dir_all(root.join("services/docs")).unwrap();
        // A plain folder with a stray `.git` file that is not a repository.
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/.git"), "gitdir: /nowhere\n").unwrap();

        let found: Vec<String> = nested_repositories(root)
            .into_iter()
            .map(|r| r.relative)
            .collect();
        assert_eq!(
            found,
            ["", "rusty-core", "services/payments/api", "services/web"]
        );
    }

    #[test]
    fn a_folder_that_is_not_a_repository_still_has_its_repositories() {
        let dir = tempfile::tempdir().unwrap();
        init(&dir.path().join("group/a"));
        init(&dir.path().join("b"));
        let found: Vec<String> = nested_repositories(dir.path())
            .into_iter()
            .map(|r| r.relative)
            .collect();
        assert_eq!(found, ["b", "group/a"]);
    }
}

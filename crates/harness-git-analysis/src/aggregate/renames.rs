//! Maps every change to the path its file has at `HEAD`.
use std::collections::HashMap;

use harness_workspace::repository_intelligence::RootRelativePath;

use crate::model::{ChangeKind, CommitFact};

/// For each commit (newest first), the `HEAD` path of each of its changes, in change order.
///
/// Walking newest to oldest, a rename makes the old name an alias for the new one in
/// older commits; adding a path ends the alias, because older history under that
/// name belonged to a different file.
pub(super) fn head_paths(commits: &[CommitFact]) -> Vec<Vec<RootRelativePath>> {
    let mut alias: HashMap<RootRelativePath, RootRelativePath> = HashMap::new();
    commits
        .iter()
        .map(|commit| {
            let resolved = commit
                .changes
                .iter()
                .map(|change| {
                    alias
                        .get(&change.path)
                        .cloned()
                        .unwrap_or_else(|| change.path.clone())
                })
                .collect::<Vec<_>>();
            for (change, head_path) in commit.changes.iter().zip(&resolved) {
                match (change.kind, &change.previous_path) {
                    (ChangeKind::Renamed, Some(previous)) => {
                        alias.insert(previous.clone(), head_path.clone());
                    }
                    (ChangeKind::Added, _) => {
                        alias.remove(&change.path);
                    }
                    _ => {}
                }
            }
            resolved
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommitId, Contributor, ContributorId, FileChange};

    fn path(value: &str) -> RootRelativePath {
        RootRelativePath::new(value).unwrap()
    }

    fn commit(id: &str, changes: Vec<FileChange>) -> CommitFact {
        CommitFact {
            id: CommitId(id.into()),
            parent_count: 1,
            author_time: 0,
            contributor: Contributor {
                id: ContributorId("c".into()),
                name: "c".into(),
            },
            subject: String::new(),
            changes,
        }
    }

    fn change(kind: ChangeKind, path_: &str, previous: Option<&str>) -> FileChange {
        FileChange {
            path: path(path_),
            previous_path: previous.map(path),
            kind,
            lines_added: Some(1),
            lines_removed: Some(0),
        }
    }

    #[test]
    fn history_follows_a_file_through_renames() {
        // Newest first: b -> c, a -> b, edit a, add a.
        let commits = vec![
            commit("4", vec![change(ChangeKind::Renamed, "c", Some("b"))]),
            commit("3", vec![change(ChangeKind::Renamed, "b", Some("a"))]),
            commit("2", vec![change(ChangeKind::Modified, "a", None)]),
            commit("1", vec![change(ChangeKind::Added, "a", None)]),
        ];
        let mapped = head_paths(&commits);
        assert!(mapped.iter().all(|paths| paths == &vec![path("c")]));
    }

    #[test]
    fn a_re_added_path_is_a_different_file() {
        // Newest first: re-add a, delete a, edit a.
        let commits = vec![
            commit("3", vec![change(ChangeKind::Added, "a", None)]),
            commit("2", vec![change(ChangeKind::Deleted, "a", None)]),
            commit("1", vec![change(ChangeKind::Modified, "a", None)]),
        ];
        let mapped = head_paths(&commits);
        assert_eq!(mapped[0], vec![path("a")]);
        assert_eq!(mapped[2], vec![path("a")]);
    }
}

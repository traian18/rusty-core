//! Resolves what the caller named to something the modes can answer about, or to the reason they
//! cannot: not supported, not indexed, changed since the snapshot, or not there at all.
use std::sync::Arc;

use harness_workspace::{
    dependency_groups::Group,
    folder_tree,
    import_graph::{IndexedFile, LanguageProfile},
    repository_intelligence::{
        ignored_directory, known_unsupported_language, PathState, RepositoryGraph,
        RepositorySnapshot, RootRelativePath,
    },
};

use super::facts::path_text;

/// Why a path that exists is not part of the answer.
pub(super) enum Absent {
    /// The language has no import support; `language` names it when the extension is known.
    Unsupported {
        language: Option<&'static str>,
    },
    /// Left out of the snapshot on purpose or by a limit.
    NotIndexed {
        reason: String,
    },
    /// Supported, but added since the snapshot was built.
    Added,
    NotFound,
}

pub(super) enum Target {
    File {
        path: RootRelativePath,
        indexed: IndexedFile,
        /// The file's content or presence differs from the snapshot; says how.
        changed: Option<&'static str>,
    },
    Folder(Group),
    Absent {
        path: String,
        why: Absent,
        profile: Option<LanguageProfile>,
    },
}

/// Resolves one `target` string against a snapshot and the disk.
pub(super) fn resolve(
    graph: &Arc<dyn RepositoryGraph>,
    snapshot: &RepositorySnapshot,
    raw: &str,
) -> Result<Target, crate::failure::Failure> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." || raw == "/" {
        return Ok(root_folder(snapshot, raw));
    }
    if raw.starts_with(folder_tree::DIRECTORY_PREFIX) || raw.starts_with(folder_tree::REST_PREFIX) {
        return Ok(
            folder_tree::group_of(snapshot, raw).map_or_else(|| missing(raw), Target::Folder)
        );
    }
    if let Some(group) = snapshot
        .groups
        .group(&harness_workspace::dependency_groups::GroupId(
            raw.to_owned(),
        ))
    {
        return Ok(Target::Folder(group.clone()));
    }
    let path = crate::args::file(raw.trim_end_matches('/'))?;
    if let Some(indexed) = snapshot.imports.files.iter().find(|file| file.path == path) {
        let changed = match graph.path_state(&path) {
            PathState::File { content_hash } if content_hash != indexed.content_hash => {
                Some("content changed since the snapshot")
            }
            PathState::Missing => Some("deleted since the snapshot"),
            _ => None,
        };
        return Ok(Target::File {
            path,
            indexed: indexed.clone(),
            changed,
        });
    }
    Ok(resolve_unindexed(graph, snapshot, path))
}

fn missing(raw: &str) -> Target {
    Target::Absent {
        path: raw.to_owned(),
        why: Absent::NotFound,
        profile: None,
    }
}

fn root_folder(snapshot: &RepositorySnapshot, raw: &str) -> Target {
    folder_tree::group_of(snapshot, "dir:").map_or_else(|| missing(raw), Target::Folder)
}

fn resolve_unindexed(
    graph: &Arc<dyn RepositoryGraph>,
    snapshot: &RepositorySnapshot,
    path: RootRelativePath,
) -> Target {
    let text = path_text(&path);
    let absent = |why, profile| Target::Absent {
        path: text.clone(),
        why,
        profile,
    };
    let state = graph.path_state(&path);
    if state == PathState::Missing {
        return absent(Absent::NotFound, None);
    }
    if state == PathState::Directory {
        let id = format!("{}{text}", folder_tree::DIRECTORY_PREFIX);
        return match folder_tree::group_of(snapshot, &id) {
            Some(group) => Target::Folder(group),
            None => absent(
                Absent::NotIndexed {
                    reason: ignored_directory(path.as_path()).map_or_else(
                        || "no indexed files under this folder".to_owned(),
                        |name| format!("under the ignored directory `{name}`"),
                    ),
                },
                None,
            ),
        };
    }
    let profile = graph.classify(&path);
    if let Some(name) = ignored_directory(path.as_path()) {
        return absent(
            Absent::NotIndexed {
                reason: format!("under the ignored directory `{name}`"),
            },
            profile,
        );
    }
    match profile {
        Some(_) => absent(Absent::Added, profile),
        None => absent(
            Absent::Unsupported {
                language: known_unsupported_language(path.as_path()),
            },
            None,
        ),
    }
}

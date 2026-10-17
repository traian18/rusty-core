use std::path::{Component, Path, PathBuf};

use harness_workspace::repository_intelligence::RootRelativePath;

/// Joins `relative` onto `base`, resolving `.` and `..`; `None` if it would leave the root.
pub(crate) fn join_normalized(base: &Path, relative: &str) -> Option<RootRelativePath> {
    let mut parts: Vec<PathBuf> = base
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(PathBuf::from(part)),
            _ => None,
        })
        .collect();
    for component in Path::new(relative).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(PathBuf::from(part)),
            _ => return None,
        }
    }
    RootRelativePath::new(parts.iter().collect::<PathBuf>()).ok()
}

pub(crate) fn file_name(path: &RootRelativePath) -> &str {
    path.as_path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

pub(crate) fn parent(path: &RootRelativePath) -> &Path {
    path.as_path().parent().unwrap_or(Path::new(""))
}

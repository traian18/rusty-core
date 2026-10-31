//! Keeps every path a tool names inside the workspace root.
//!
//! The policy is "no symlinks at all": an existing symlink anywhere on the path is refused
//! instead of being resolved and compared with the root. Resolving needs the target to exist, so a
//! dangling link such as `escape.txt -> /outside/not-created-yet` would pass a resolve-and-compare
//! check and still be followed by the write that comes next. Refusing links fails closed and leaves
//! no gap between checking and using the path.

use std::path::{Component, Path, PathBuf};

use tokio::fs;

use crate::error::WorkspaceError;

/// Resolves `relative` under `root` by path arithmetic alone, with no filesystem access. Fails
/// when the result would leave the root or the path is absolute.
pub(super) fn resolve_lexically(root: &Path, relative: &str) -> Result<PathBuf, WorkspaceError> {
    let mut resolved = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !step_up(&mut resolved, root) {
                    return Err(escaped(root, relative));
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(escaped(root, relative)),
        }
    }
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(escaped(root, relative))
    }
}

/// Like [`resolve_lexically`], and also refuses a path through any symlink that already exists.
/// The lexical check alone cannot see a link inside the workspace that points outside it.
pub(super) async fn resolve_without_symlinks(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, WorkspaceError> {
    let resolved = resolve_lexically(root, relative)?;
    let below_root = resolved.strip_prefix(root).unwrap_or(&resolved);
    let mut current = root.to_path_buf();
    for component in below_root.components() {
        current.push(component);
        match fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(escaped(root, relative));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(resolved)
}

/// Moves up one level; false when there is nowhere to go or the new position is neither the root
/// nor one of its ancestors.
fn step_up(resolved: &mut PathBuf, root: &Path) -> bool {
    resolved.pop() && (root.starts_with(&*resolved) || root == resolved.as_path())
}

fn escaped(root: &Path, relative: &str) -> WorkspaceError {
    WorkspaceError::PathTraversal {
        path: root.join(relative),
    }
}

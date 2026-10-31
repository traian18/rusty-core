//! Writing a file so that no reader ever sees half of it.

use std::path::{Path, PathBuf};

use tokio::fs;
use uuid::Uuid;

use super::path_guard::{resolve_lexically, resolve_without_symlinks};
use crate::error::WorkspaceError;

/// The final location for `relative`, with its parent directories created.
///
/// Parents are created before the symlink check so that the check runs against a path whose
/// directories all exist.
pub(super) async fn prepare_target(root: &Path, relative: &str) -> Result<PathBuf, WorkspaceError> {
    let lexical = resolve_lexically(root, relative)?;
    if let Some(parent) = lexical.parent() {
        fs::create_dir_all(parent).await?;
    }
    resolve_without_symlinks(root, relative).await
}

/// Writes to a temporary file next to `target` and renames it into place. The rename is atomic
/// on one filesystem, so a crash or a concurrent reader never observes a partial file.
pub(super) async fn write_atomically(
    root: &Path,
    target: &Path,
    content: &str,
) -> Result<(), WorkspaceError> {
    let directory = target.parent().unwrap_or(root);
    let temporary = directory.join(temporary_name(target));
    fs::write(&temporary, content).await?;
    if let Err(error) = fs::rename(&temporary, target).await {
        let _ = fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

fn temporary_name(target: &Path) -> String {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("write");
    format!(".{file_name}.tmp-{}", Uuid::new_v4())
}

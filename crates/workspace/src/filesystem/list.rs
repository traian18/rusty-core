//! Recursive directory listing.

use std::{future::Future, path::Path, pin::Pin};

use tokio::fs;

use crate::{error::WorkspaceError, listing::FileInfo};

type ListFuture<'a> = Pin<Box<dyn Future<Output = Result<(), WorkspaceError>> + Send + 'a>>;

/// Lists everything under `root` down to `max_depth` levels; 0 means no limit.
pub(super) async fn list_tree(
    root: &Path,
    max_depth: usize,
) -> Result<Vec<FileInfo>, WorkspaceError> {
    let mut files = Vec::new();
    list_directory(root, 0, max_depth, &mut files).await?;
    Ok(files)
}

/// Symlinks are skipped entirely. With `max_depth == 0` nothing else would stop a link back to
/// an ancestor from looping forever.
fn list_directory<'a>(
    directory: &'a Path,
    depth: usize,
    max_depth: usize,
    out: &'a mut Vec<FileInfo>,
) -> ListFuture<'a> {
    Box::pin(async move {
        if max_depth > 0 && depth > max_depth {
            return Ok(());
        }
        let mut entries = fs::read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_symlink() {
                continue;
            }
            let metadata = entry.metadata().await?;
            let path = entry.path();
            out.push(FileInfo {
                path: path.strip_prefix(directory).unwrap_or(&path).to_path_buf(),
                size_bytes: metadata.len(),
                is_directory: metadata.is_dir(),
            });
            if metadata.is_dir() {
                list_directory(&path, depth + 1, max_depth, out).await?;
            }
        }
        Ok(())
    })
}

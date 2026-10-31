//! Reading a file without letting its size decide how much memory the host uses.

use std::path::Path;

use tokio::{fs, io::AsyncReadExt};

use crate::error::WorkspaceError;

/// A file larger than this is read only up to the limit and marked as truncated.
pub(super) const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

const TRUNCATION_NOTICE: &str = "\n... (truncated, exceeds read size limit)";

/// Reads at most [`MAX_READ_BYTES`] of `path` as text, replacing invalid UTF-8.
pub(super) async fn read_capped(path: &Path) -> Result<String, WorkspaceError> {
    let mut file = fs::File::open(path).await?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_READ_BYTES)
        .read_to_end(&mut bytes)
        .await?;
    let exceeded_limit = file.read(&mut [0u8; 1]).await? > 0;

    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if exceeded_limit {
        text.push_str(TRUNCATION_NOTICE);
    }
    Ok(text)
}

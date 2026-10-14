use std::io::Read;
use std::path::Path;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::CallGraphError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub hash: String,
}

#[async_trait]
pub trait AnalysisSource: Send + Sync {
    async fn discover(
        &self,
        root: &Path,
        max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<SourceFile>, CallGraphError>;
}

pub struct FileSystemSource {
    max_bytes: usize,
    max_total_bytes: usize,
    max_depth: usize,
}

impl FileSystemSource {
    pub fn new(max_bytes: usize, max_depth: usize) -> Self {
        Self {
            max_bytes,
            max_total_bytes: max_bytes.saturating_mul(16),
            max_depth,
        }
    }

    fn visit(
        &self,
        root: &Path,
        dir: &Path,
        depth: usize,
        limit: usize,
        cancel: &CancellationToken,
        files: &mut Vec<SourceFile>,
    ) -> Result<(), CallGraphError> {
        if cancel.is_cancelled() {
            return Err(CallGraphError::Cancelled);
        }
        if depth > self.max_depth {
            return Err(CallGraphError::LimitExceeded(
                "source traversal depth".into(),
            ));
        }
        let entries =
            std::fs::read_dir(dir).map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
        for entry in entries {
            if files.len() >= limit {
                return Err(CallGraphError::LimitExceeded("source file count".into()));
            }
            let entry = entry.map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
            let path = entry.path();
            let before = std::fs::symlink_metadata(&path)
                .map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
            if before.file_type().is_symlink() {
                continue;
            }
            if before.is_dir() {
                self.visit(root, &path, depth + 1, limit, cancel, files)?;
                continue;
            }
            if !before.is_file() || !is_source(&path) {
                continue;
            }
            if before.len() as usize > self.max_bytes {
                return Err(CallGraphError::LimitExceeded(format!(
                    "source file size: {}",
                    path.display()
                )));
            }
            let (bytes, opened) = read_without_following(&path)?;
            let after = std::fs::symlink_metadata(&path)
                .map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
            if !same_file_metadata(&before, &after)
                || !same_file_metadata(&before, &opened)
                || after.file_type().is_symlink()
            {
                return Err(CallGraphError::DependencyFailure(
                    "source changed while it was being read".into(),
                ));
            }
            let total_bytes = files.iter().map(|file| file.bytes.len()).sum::<usize>();
            if total_bytes.saturating_add(bytes.len()) > self.max_total_bytes {
                return Err(CallGraphError::LimitExceeded("total source bytes".into()));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| CallGraphError::InvalidRequest("path escaped root".into()))?;
            files.push(SourceFile {
                path: relative.to_string_lossy().replace('\\', "/"),
                hash: hash_bytes(&bytes),
                bytes,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl AnalysisSource for FileSystemSource {
    async fn discover(
        &self,
        root: &Path,
        max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<SourceFile>, CallGraphError> {
        if !root.is_absolute() {
            return Err(CallGraphError::InvalidRequest(
                "root must be absolute".into(),
            ));
        }
        let root = std::fs::canonicalize(root)
            .map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
        let mut files = Vec::new();
        self.visit(&root, &root, 0, max_files, cancel, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }
}

fn read_without_following(path: &Path) -> Result<(Vec<u8>, std::fs::Metadata), CallGraphError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT opens the link itself instead of its target.
        // The subsequent regular-file check rejects reparse points without traversing them.
        options.custom_flags(0x0020_0000);
    }
    let mut file = options
        .open(path)
        .map_err(|error| CallGraphError::DependencyFailure(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| CallGraphError::DependencyFailure(error.to_string()))?;
    if !metadata.is_file() {
        return Err(CallGraphError::InvalidRequest(
            "source is not a regular file".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| CallGraphError::DependencyFailure(error.to_string()))?;
    Ok((bytes, metadata))
}

fn same_file_metadata(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.file_type() == after.file_type()
}
fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "js" | "jsx"
                | "ts"
                | "tsx"
                | "py"
                | "java"
                | "cs"
                | "go"
                | "rs"
                | "php"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "rb"
        )
    )
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hash = 1469598103934665603u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

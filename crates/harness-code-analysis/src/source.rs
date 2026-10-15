use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::CallGraphError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceDiscovery {
    pub files: Vec<SourceFile>,
    pub diagnostics: Vec<crate::Diagnostic>,
    pub limits: Vec<String>,
}

#[async_trait]
pub trait AnalysisSource: Send + Sync {
    async fn discover(
        &self,
        root: &Path,
        max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError>;
}

/// Resource limits applied while discovering source.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_bytes: usize,
    pub max_total_bytes: usize,
    pub max_depth: usize,
}

impl Limits {
    pub(crate) fn describe(&self, max_files: usize) -> Vec<String> {
        vec![
            format!("max_files={max_files}"),
            format!("max_file_bytes={}", self.max_bytes),
            format!("max_total_bytes={}", self.max_total_bytes),
            format!("max_depth={}", self.max_depth),
        ]
    }
}

/// Test hook invoked with a root-relative path after a file was listed and before it is opened.
pub type OpenHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Bounded filesystem source.
///
/// Containment and race resistance rely on directory-handle traversal (`openat` with
/// `O_NOFOLLOW`), so discovery is implemented for Unix only. On other platforms (Windows is not
/// supported for this feature) discovery refuses with an explicit error instead of scanning
/// with weaker guarantees.
#[derive(Clone)]
pub struct FileSystemSource {
    max_bytes: usize,
    max_total_bytes: usize,
    max_depth: usize,
    hook: Option<OpenHook>,
}

impl FileSystemSource {
    pub fn new(max_bytes: usize, max_depth: usize) -> Self {
        Self {
            max_bytes,
            max_total_bytes: max_bytes.saturating_mul(16),
            max_depth,
            hook: None,
        }
    }

    /// Installs a hook run between listing and opening each file. Intended for race tests.
    #[doc(hidden)]
    pub fn with_open_hook(mut self, hook: OpenHook) -> Self {
        self.hook = Some(hook);
        self
    }

    fn limits(&self) -> Limits {
        Limits {
            max_bytes: self.max_bytes,
            max_total_bytes: self.max_total_bytes,
            max_depth: self.max_depth,
        }
    }

    pub fn with_total_byte_limit(mut self, max_total_bytes: usize) -> Self {
        self.max_total_bytes = max_total_bytes;
        self
    }
}

#[async_trait]
impl AnalysisSource for FileSystemSource {
    async fn discover(
        &self,
        root: &Path,
        max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError> {
        if !root.is_absolute() {
            return Err(CallGraphError::InvalidRequest(
                "root must be absolute".into(),
            ));
        }
        let root = std::fs::canonicalize(root)
            .map_err(|e| CallGraphError::DependencyFailure(e.to_string()))?;
        let source = self.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || source.scan_blocking(&root, max_files, &cancel))
            .await
            .map_err(|error| {
                CallGraphError::DependencyFailure(format!("scan task failed: {error}"))
            })?
    }
}

impl FileSystemSource {
    #[cfg(not(unix))]
    fn scan_blocking(
        &self,
        _root: &Path,
        _max_files: usize,
        _cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError> {
        Err(CallGraphError::DependencyFailure(
            "filesystem call-graph analysis is only supported on Unix platforms".into(),
        ))
    }

    #[cfg(unix)]
    fn scan_blocking(
        &self,
        root: &Path,
        max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError> {
        crate::source_unix::scan(root, &self.limits(), max_files, cancel, self.hook.as_ref())
    }
}

pub(crate) fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "js" | "jsx"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "py"
                | "java"
                | "cs"
                | "go"
                | "rs"
                | "php"
                | "c"
                | "cc"
                | "cpp"
                | "cxx"
                | "h"
                | "hh"
                | "hpp"
                | "hxx"
                | "rb"
        )
    )
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

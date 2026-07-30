//! In-memory [`Workspace`](crate::traits::Workspace) stub for Phase 2 (spec §68.2).
//!
//! [`FakeWorkspace`] holds files entirely in memory behind a mutex. It exists
//! so that [`SessionRuntime`](crate::session_runtime::SessionRuntime) always
//! has a concrete `Arc<dyn Workspace>` to bind without requiring a real
//! filesystem implementation, which arrives in Phase 4.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::traits::{SearchMatch, SearchQuery, SearchResult, Workspace, WorkspaceError, WorkspaceStatus};

/// A purely in-memory [`Workspace`] implementation used for tests and as the
/// default Phase 2 workspace binding.
#[derive(Debug, Default)]
pub struct FakeWorkspace {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    root: Option<PathBuf>,
    writable: bool,
}

impl FakeWorkspace {
    /// Creates an empty, writable fake workspace with no configured root.
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            root: None,
            writable: true,
        }
    }

    /// Sets the workspace root reported by [`Workspace::status`].
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Seeds the workspace with a file, available for `read`/`search` before
    /// any command is sent.
    pub fn with_file(self, path: impl Into<PathBuf>, data: impl Into<Vec<u8>>) -> Self {
        self.files
            .lock()
            .expect("files mutex poisoned")
            .insert(path.into(), data.into());
        self
    }

    /// Marks the workspace as read-only; `write` calls will fail with
    /// [`WorkspaceError::PermissionDenied`].
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }
}

#[async_trait]
impl Workspace for FakeWorkspace {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, WorkspaceError> {
        self.files
            .lock()
            .expect("files mutex poisoned")
            .get(path)
            .cloned()
            .ok_or_else(|| WorkspaceError::NotFound(path.display().to_string()))
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), WorkspaceError> {
        if !self.writable {
            return Err(WorkspaceError::PermissionDenied(path.display().to_string()));
        }
        self.files
            .lock()
            .expect("files mutex poisoned")
            .insert(path.to_path_buf(), data.to_vec());
        Ok(())
    }

    async fn search(&self, query: SearchQuery) -> Result<SearchResult, WorkspaceError> {
        let files = self.files.lock().expect("files mutex poisoned");
        let mut matches = Vec::new();

        for (path, contents) in files.iter() {
            if let Some(prefix) = &query.path_prefix {
                if !path.starts_with(prefix) {
                    continue;
                }
            }

            let text = String::from_utf8_lossy(contents);
            for (idx, line) in text.lines().enumerate() {
                if line.contains(&query.pattern) {
                    matches.push(SearchMatch {
                        path: path.clone(),
                        line: (idx + 1) as u64,
                        preview: line.chars().take(200).collect(),
                    });
                    if let Some(max) = query.max_results {
                        if matches.len() >= max {
                            return Ok(SearchResult { matches });
                        }
                    }
                }
            }
        }

        Ok(SearchResult { matches })
    }

    async fn status(&self) -> Result<WorkspaceStatus, WorkspaceError> {
        let files = self.files.lock().expect("files mutex poisoned");
        Ok(WorkspaceStatus {
            root: self.root.clone(),
            file_count: files.len(),
            is_writable: self.writable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let ws = FakeWorkspace::new();
        let path = PathBuf::from("/src/main.rs");
        let content: &[u8] = b"fn main() {}";
        ws.write(&path, content).await.expect("write");
        let data = ws.read(&path).await.expect("read");
        assert_eq!(data, content.to_vec());
    }

    #[tokio::test]
    async fn read_missing_file_is_not_found() {
        let ws = FakeWorkspace::new();
        let err = ws.read(Path::new("/missing.txt")).await.unwrap_err();
        assert!(matches!(err, WorkspaceError::NotFound(_)));
    }

    #[tokio::test]
    async fn read_only_workspace_rejects_writes() {
        let ws = FakeWorkspace::new().read_only();
        let content: &[u8] = b"x";
        let err = ws.write(Path::new("/a.txt"), content).await.unwrap_err();
        assert!(matches!(err, WorkspaceError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn search_finds_seeded_content() {
        let content: &[u8] = b"pub fn parse() {}\nfn helper() {}";
        let ws = FakeWorkspace::new().with_file("/src/lib.rs", content);
        let result = ws
            .search(SearchQuery {
                pattern: "parse".into(),
                path_prefix: None,
                max_results: None,
            })
            .await
            .expect("search");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, PathBuf::from("/src/lib.rs"));
        assert_eq!(result.matches[0].line, 1);
    }

    #[tokio::test]
    async fn status_reports_file_count_and_writability() {
        let a: &[u8] = b"a";
        let b: &[u8] = b"b";
        let ws = FakeWorkspace::new()
            .with_root("/project")
            .with_file("/project/a.txt", a)
            .with_file("/project/b.txt", b);
        let status = ws.status().await.expect("status");
        assert_eq!(status.root, Some(PathBuf::from("/project")));
        assert_eq!(status.file_count, 2);
        assert!(status.is_writable);
    }
}

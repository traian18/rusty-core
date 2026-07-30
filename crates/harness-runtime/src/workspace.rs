//! In-memory [`Workspace`] stub for Phase 2 (spec §68.2).
//!
//! [`FakeWorkspace`] holds files entirely in memory behind a mutex. It exists
//! so that [`SessionRuntime`] always has a concrete `Arc<dyn Workspace>` to
//! bind without requiring a real filesystem implementation, which arrives in Phase 4.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use harness_workspace::{
    FileInfo, SearchMatch, SearchResult, Workspace, WorkspaceError, WorkspaceMode,
};

/// A purely in-memory [`Workspace`] implementation used for tests and as the
/// default Phase 2 workspace binding.
#[derive(Debug, Default)]
pub struct FakeWorkspace {
    files: Mutex<HashMap<String, String>>,
    root: PathBuf,
    writable: bool,
}

impl FakeWorkspace {
    /// Creates an empty, writable fake workspace with a default root.
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            root: PathBuf::from("/fake"),
            writable: true,
        }
    }

    /// Sets the workspace root reported by [`Workspace::root`].
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Seeds the workspace with a file, available for `read`/`search` before
    /// any command is sent.
    pub fn with_file(self, path: impl Into<String>, content: impl Into<String>) -> Self {
        self.files
            .lock()
            .expect("files mutex poisoned")
            .insert(path.into(), content.into());
        self
    }

    /// Marks the workspace as read-only; `write` calls will fail.
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }
}

#[async_trait]
impl Workspace for FakeWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Shared
    }

    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError> {
        self.files
            .lock()
            .expect("files mutex poisoned")
            .get(relative_path)
            .cloned()
            .ok_or_else(|| WorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("file not found: {}", relative_path),
            )))
    }

    async fn write(&self, relative_path: &str, content: &str) -> Result<(), WorkspaceError> {
        if !self.writable {
            return Err(WorkspaceError::Isolated);
        }
        self.files
            .lock()
            .expect("files mutex poisoned")
            .insert(relative_path.to_string(), content.to_string());
        Ok(())
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        let files = self.files.lock().expect("files mutex poisoned");
        let mut matches = Vec::new();
        let mut total_count = 0;

        for (path, contents) in files.iter() {
            for (idx, line) in contents.lines().enumerate() {
                if line.contains(query) {
                    total_count += 1;
                    matches.push(SearchMatch {
                        file_path: PathBuf::from(path),
                        line_number: idx + 1,
                        line_content: line.to_string(),
                    });
                }
            }
        }

        Ok(SearchResult { matches, total_count })
    }

    async fn list_files(&self, _max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        let files = self.files.lock().expect("files mutex poisoned");
        let file_infos = files
            .iter()
            .map(|(path, content)| FileInfo {
                path: PathBuf::from(path),
                size_bytes: content.len() as u64,
                is_directory: false,
            })
            .collect();
        Ok(file_infos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let ws = FakeWorkspace::new();
        let path = "src/main.rs";
        let content = "fn main() {}";
        ws.write(path, content).await.expect("write");
        let data = ws.read(path).await.expect("read");
        assert_eq!(data, content);
    }

    #[tokio::test]
    async fn read_missing_file_errors() {
        let ws = FakeWorkspace::new();
        let err = ws.read("missing.txt").await.unwrap_err();
        assert!(matches!(err, WorkspaceError::Io(_)));
    }

    #[tokio::test]
    async fn read_only_workspace_rejects_writes() {
        let ws = FakeWorkspace::new().read_only();
        let err = ws.write("a.txt", "x").await.unwrap_err();
        assert!(matches!(err, WorkspaceError::Isolated));
    }

    #[tokio::test]
    async fn search_finds_seeded_content() {
        let content = "pub fn parse() {}\nfn helper() {}";
        let ws = FakeWorkspace::new().with_file("src/lib.rs", content);
        let result = ws.search("parse").await.expect("search");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("src/lib.rs"));
        assert_eq!(result.matches[0].line_number, 1);
    }

    #[tokio::test]
    async fn root_and_mode_reported_correctly() {
        let ws = FakeWorkspace::new().with_root("/project");
        assert_eq!(ws.root(), Path::new("/project"));
        assert_eq!(ws.mode(), WorkspaceMode::Shared);
    }
}

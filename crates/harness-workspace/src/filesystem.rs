use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::workspace::{Workspace, WorkspaceError, SearchResult, SearchMatch, FileInfo};

/// Filesystem-backed workspace. All paths are resolved relative to `root`.
///
/// Path traversal defense: any `..` component that would escape `root`
/// returns `WorkspaceError::PathTraversal`.
pub struct FsWorkspace {
    root: PathBuf,
    mode: crate::workspace::WorkspaceMode,
}

impl FsWorkspace {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mode: crate::workspace::WorkspaceMode::Shared,
        }
    }

    pub fn with_mode(mut self, mode: crate::workspace::WorkspaceMode) -> Self {
        self.mode = mode;
        self
    }

    /// Resolve a relative path to an absolute path rooted at `self.root`.
    ///
    /// Returns `WorkspaceError::PathTraversal` if the path contains components
    /// that would escape the workspace root.
    fn resolve_path(&self, relative: &str) -> Result<PathBuf, WorkspaceError> {
        // Start with the root directory
        let mut normalized = self.root.clone();

        // Process each component of the relative path
        for component in Path::new(relative).components() {
            match component {
                Component::ParentDir => {
                    // Going up one level - check if we'd escape the root
                    if normalized.pop() {
                        // Successfully popped a component
                        // Check if we're still within or at root
                        if !self.root.starts_with(&normalized) && self.root != normalized {
                            return Err(WorkspaceError::PathTraversal {
                                path: self.root.join(relative),
                            });
                        }
                    } else {
                        // Couldn't pop (already at filesystem root)
                        return Err(WorkspaceError::PathTraversal {
                            path: self.root.join(relative),
                        });
                    }
                }
                Component::Normal(name) => {
                    // Regular path component - add it
                    normalized.push(name);
                }
                Component::CurDir => {
                    // Current dir - do nothing
                }
                Component::RootDir | Component::Prefix(_) => {
                    // Absolute paths or drive letters are not allowed
                    return Err(WorkspaceError::PathTraversal {
                        path: self.root.join(relative),
                    });
                }
            }
        }

        // Final check: ensure we're still within root
        if !normalized.starts_with(&self.root) {
            return Err(WorkspaceError::PathTraversal {
                path: self.root.join(relative),
            });
        }

        Ok(normalized)
    }
}

#[async_trait::async_trait]
impl Workspace for FsWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn mode(&self) -> crate::workspace::WorkspaceMode {
        self.mode
    }

    async fn read(&self, relative_path: &str) -> Result<String, WorkspaceError> {
        let absolute = self.resolve_path(relative_path)?;
        let mut file = fs::File::open(&absolute).await?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;
        Ok(contents)
    }

    async fn write(&self, relative_path: &str, content: &str) -> Result<(), WorkspaceError> {
        if self.mode == crate::workspace::WorkspaceMode::Isolated {
            return Err(WorkspaceError::Isolated);
        }

        let absolute = self.resolve_path(relative_path)?;

        // Ensure parent directory exists.
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&absolute, content).await?;
        info!(path = %relative_path, "wrote file to workspace");
        Ok(())
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        let mut matches = Vec::new();
        let query_lower = query.to_lowercase();

        Self::search_dir_impl(&self.root, &self.root, &query_lower, &mut matches).await?;

        matches.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.line_number.cmp(&b.line_number))
        });

        Ok(SearchResult {
            total_count: matches.len(),
            matches,
        })
    }

    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        let mut files = Vec::new();
        Self::list_dir_impl(&self.root, 0, max_depth, &mut files).await?;
        Ok(files)
    }
}

impl FsWorkspace {
    /// Recursively walk a directory, searching for `query` in UTF-8 text files.
    fn search_dir_impl<'a>(
        root: &'a Path,
        dir: &'a Path,
        query: &'a str,
        matches: &'a mut Vec<SearchMatch>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), WorkspaceError>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let meta = entry.metadata().await?;

                if meta.is_dir() {
                    Self::search_dir_impl(root, &path, query, matches).await?;
                } else if meta.is_file() {
                    if let Some(ext) = path.extension() {
                        let text_ext = matches!(
                            ext.to_string_lossy().as_ref(),
                            "txt" | "rs" | "toml" | "yaml" | "yml" | "json" | "md"
                                | "sh" | "py" | "js" | "ts" | "lock" | "cfg"
                                | "ini" | "conf" | "env" | "log" | "csv" | "html"
                                | "css" | "xml" | "svg"
                        );

                        if !text_ext {
                            continue;
                        }
                    }

                    if let Ok(contents) = fs::read_to_string(&path).await {
                        for (idx, line) in contents.lines().enumerate() {
                            if line.to_lowercase().contains(query) {
                                matches.push(SearchMatch {
                                    file_path: path
                                        .strip_prefix(root)
                                        .unwrap_or(&path)
                                        .to_path_buf(),
                                    line_number: idx + 1,
                                    line_content: line.to_string(),
                                });
                            }
                        }
                    }
                }
            }
            Ok(())
        })
    }

    fn list_dir_impl<'a>(
        dir: &'a Path,
        depth: usize,
        max_depth: usize,
        out: &'a mut Vec<FileInfo>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), WorkspaceError>> + Send + 'a>> {
        Box::pin(async move {
            if max_depth > 0 && depth > max_depth {
                return Ok(());
            }

            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let meta = entry.metadata().await?;
                let path = entry.path();
                let rel = path.strip_prefix(dir).unwrap_or(&path);

                out.push(FileInfo {
                    path: rel.to_path_buf(),
                    size_bytes: meta.len(),
                    is_directory: meta.is_dir(),
                });

                if meta.is_dir() {
                    Self::list_dir_impl(&path, depth + 1, max_depth, out).await?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn fs_workspace_read_write_roundtrip() {
        let dir = tempdir().unwrap();
        let ws = FsWorkspace::new(dir.path().to_path_buf());

        ws.write("hello.txt", "world\n").await.unwrap();
        let contents = ws.read("hello.txt").await.unwrap();
        assert_eq!(contents, "world\n");
    }

    #[tokio::test]
    async fn fs_workspace_blocks_path_traversal() {
        let dir = tempdir().unwrap();
        let ws = FsWorkspace::new(dir.path().to_path_buf());

        let result = ws.read("../../etc/passwd").await;
        assert!(matches!(result, Err(WorkspaceError::PathTraversal { .. })));
    }

    #[tokio::test]
    async fn fs_workspace_search_finds_matches() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("greeting.rs"),
            "fn hello() { println!(\"hi\"); }",
        )
        .await
        .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = ws.search("println").await.unwrap();

        assert_eq!(result.total_count, 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("greeting.rs"));
        assert_eq!(result.matches[0].line_number, 1);
    }
}

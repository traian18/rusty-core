use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::workspace::{FileInfo, SearchMatch, SearchResult, Workspace, WorkspaceError};

/// M3: caps a single `read` from growing the host process's memory
/// unbounded on an adversarial or merely huge file. Mirrors the truncation
/// pattern used by `harness-tool-git`'s `MAX_DIFF_BYTES` and
/// `harness-tool-web`'s `read_capped`.
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

/// M3: caps how many files a single `search` call will scan, so a workspace
/// with an enormous tree can't turn one tool call into an unbounded
/// traversal.
const MAX_SEARCH_FILES_SCANNED: usize = 20_000;

/// M3: caps how many matches a single `search` call accumulates. Traversal
/// stops early once this is hit, rather than continuing to scan (and
/// allocate `SearchMatch` entries for) the rest of the tree.
const MAX_SEARCH_MATCHES: usize = 5_000;

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

    /// Resolves `relative` the same way as [`Self::resolve_path`], then
    /// additionally rejects the result if any *existing* path component —
    /// including the final one — is a symlink.
    ///
    /// [`Self::resolve_path`] alone is purely lexical (`Path::components()`
    /// manipulation, no filesystem access), so it cannot see a symlink that
    /// already exists inside the workspace and points outside it (e.g.
    /// `workspace/escape -> /etc`): `resolve_path("escape/passwd")` lexically
    /// starts with `root` and passes every check there, but the OS would
    /// still follow the symlink at actual `open`/`write` time and touch
    /// `/etc/passwd`.
    ///
    /// The policy here is deliberately "no symlinks at all" rather than
    /// "resolve the symlink and check whether *that* stays under root":
    /// resolving requires the target to exist ([`std::fs::canonicalize`]
    /// fails on a dangling symlink), so a broken symlink whose target
    /// doesn't exist yet — e.g. `escape.txt -> /outside/not-created-yet` —
    /// would otherwise slip through a resolve-and-compare check and still
    /// get followed by a subsequent `fs::write`. Flatly refusing to operate
    /// through any symlink is simpler, fails closed on dangling targets, and
    /// avoids a resolve-then-act TOCTOU window entirely.
    async fn resolve_and_verify_path(&self, relative: &str) -> Result<PathBuf, WorkspaceError> {
        let resolved = self.resolve_path(relative)?;

        // Walk from root down to the resolved path, checking each existing
        // component with `symlink_metadata` (lstat — does not follow
        // symlinks, so it reports on the component itself). Components that
        // don't exist yet are, by definition, not symlinks and stop the walk
        // (nothing deeper can exist either).
        let mut current = self.root.clone();
        let relative_to_root = resolved
            .strip_prefix(&self.root)
            .unwrap_or(resolved.as_path());
        for component in relative_to_root.components() {
            current.push(component);
            match fs::symlink_metadata(&current).await {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(WorkspaceError::PathTraversal {
                        path: self.root.join(relative),
                    });
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        Ok(resolved)
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
        let absolute = self.resolve_and_verify_path(relative_path).await?;
        let mut file = fs::File::open(&absolute).await?;

        // M3: cap how much of the file is actually read into memory, rather
        // than trusting file size — a huge or adversarial file must not be
        // read fully before we can decide to truncate.
        let mut limited = (&mut file).take(MAX_READ_BYTES);
        let mut buf = Vec::new();
        limited.read_to_end(&mut buf).await?;

        let truncated = file.read(&mut [0u8; 1]).await? > 0;
        let mut contents = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            contents.push_str("\n... (truncated, exceeds read size limit)");
        }
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

        // Re-verify (including symlink escape) now that the parent
        // directory is guaranteed to exist.
        let absolute = self.resolve_and_verify_path(relative_path).await?;

        // M3: write atomically (temp file in the same directory + rename)
        // instead of a direct `fs::write`, so a crash or concurrent reader
        // mid-write never observes a partial/corrupt file at the target
        // path. The temp file lives in the same directory so the rename is
        // an atomic same-filesystem operation.
        let parent = absolute.parent().unwrap_or(&self.root);
        let temp_name = format!(
            ".{}.tmp-{}",
            absolute
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("write"),
            uuid::Uuid::new_v4(),
        );
        let temp_path = parent.join(temp_name);
        fs::write(&temp_path, content).await?;
        if let Err(error) = fs::rename(&temp_path, &absolute).await {
            let _ = fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
        info!(path = %relative_path, "wrote file to workspace");
        Ok(())
    }

    async fn search(&self, query: &str) -> Result<SearchResult, WorkspaceError> {
        let mut matches = Vec::new();
        let query_lower = query.to_lowercase();
        let mut files_scanned = 0usize;

        let truncated = Self::search_dir_impl(
            &self.root,
            &self.root,
            &query_lower,
            &mut matches,
            &mut files_scanned,
        )
        .await?;

        matches.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.line_number.cmp(&b.line_number))
        });

        Ok(SearchResult {
            total_count: matches.len(),
            matches,
            truncated,
        })
    }

    async fn list_files(&self, max_depth: usize) -> Result<Vec<FileInfo>, WorkspaceError> {
        let mut files = Vec::new();
        Self::list_dir_impl(&self.root, 0, max_depth, &mut files).await?;
        Ok(files)
    }
}

impl FsWorkspace {
    /// Recursively walk a directory, searching for `query` in UTF-8 text
    /// files. Bounded by `MAX_SEARCH_FILES_SCANNED` (files visited) and
    /// `MAX_SEARCH_MATCHES` (matches collected) — M3: an enormous or
    /// adversarial workspace tree must not turn one `search` call into
    /// unbounded traversal or an unbounded `matches` allocation. Returns
    /// `true` if traversal stopped early because a cap was hit.
    fn search_dir_impl<'a>(
        root: &'a Path,
        dir: &'a Path,
        query: &'a str,
        matches: &'a mut Vec<SearchMatch>,
        files_scanned: &'a mut usize,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, WorkspaceError>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if *files_scanned >= MAX_SEARCH_FILES_SCANNED || matches.len() >= MAX_SEARCH_MATCHES
                {
                    return Ok(true);
                }

                // M3: `DirEntry::file_type()` reports on the entry itself
                // without following it (unlike `entry.metadata()`, which
                // does) — matching this workspace's existing "no symlinks
                // at all" policy (see `resolve_within_root`'s doc comment),
                // a symlinked directory is never recursed into. Without
                // this, a symlink cycle (e.g. `a/link -> a`, or one pointing
                // at any ancestor) sends this recursion into an unbounded
                // loop that neither of the caps below can catch, since
                // `files_scanned` only advances on regular files, never on
                // directory descents.
                let file_type = entry.file_type().await?;
                if file_type.is_symlink() {
                    continue;
                }

                let path = entry.path();
                let meta = entry.metadata().await?;

                if meta.is_dir() {
                    if Self::search_dir_impl(root, &path, query, matches, files_scanned).await? {
                        return Ok(true);
                    }
                } else if meta.is_file() {
                    *files_scanned += 1;

                    if let Some(ext) = path.extension() {
                        let text_ext = matches!(
                            ext.to_string_lossy().as_ref(),
                            "txt"
                                | "rs"
                                | "toml"
                                | "yaml"
                                | "yml"
                                | "json"
                                | "md"
                                | "sh"
                                | "py"
                                | "js"
                                | "ts"
                                | "lock"
                                | "cfg"
                                | "ini"
                                | "conf"
                                | "env"
                                | "log"
                                | "csv"
                                | "html"
                                | "css"
                                | "xml"
                                | "svg"
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
                                if matches.len() >= MAX_SEARCH_MATCHES {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
            }
            Ok(false)
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
                // M3: same symlink-cycle guard as `search_dir_impl` — see
                // its doc comment. Here a cycle would additionally be
                // bounded by `max_depth` when the caller passes a positive
                // one, but `max_depth == 0` means "unlimited" (see the
                // early-return above), so that alone is not a safe default;
                // never recursing into a symlinked directory at all is.
                let file_type = entry.file_type().await?;
                if file_type.is_symlink() {
                    continue;
                }

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
mod tests;

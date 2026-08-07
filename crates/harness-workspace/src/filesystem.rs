use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tokio::fs;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::workspace::{Workspace, WorkspaceError, SearchResult, SearchMatch, FileInfo};

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
        let relative_to_root = resolved.strip_prefix(&self.root).unwrap_or(resolved.as_path());
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
            absolute.file_name().and_then(|n| n.to_str()).unwrap_or("write"),
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
                if *files_scanned >= MAX_SEARCH_FILES_SCANNED || matches.len() >= MAX_SEARCH_MATCHES {
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

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_workspace_blocks_read_through_a_symlink_escaping_root() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "top secret")
            .await
            .unwrap();

        // A symlink *inside* the workspace root pointing to a directory
        // outside it. Lexically, "escape/secret.txt" starts with `root` and
        // would pass the old `resolve_path`-only check, but the OS follows
        // the symlink at actual open time and would read the outside file.
        tokio::fs::symlink(outside.path(), dir.path().join("escape"))
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = ws.read("escape/secret.txt").await;
        assert!(
            matches!(result, Err(WorkspaceError::PathTraversal { .. })),
            "reading through a symlink that escapes the workspace root must be rejected, got {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_workspace_blocks_write_through_a_symlink_escaping_root() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();

        // The leaf itself is a symlink pointing outside root.
        tokio::fs::symlink(outside.path().join("clobbered.txt"), dir.path().join("escape.txt"))
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = ws.write("escape.txt", "attacker-controlled content").await;
        assert!(
            matches!(result, Err(WorkspaceError::PathTraversal { .. })),
            "writing through a symlink that escapes the workspace root must be rejected, got {result:?}"
        );
        assert!(
            !outside.path().join("clobbered.txt").exists(),
            "the file outside the workspace root must never be created"
        );
    }

    #[tokio::test]
    async fn fs_workspace_write_is_atomic_no_partial_file_on_crash() {
        let dir = tempdir().unwrap();
        let ws = FsWorkspace::new(dir.path().to_path_buf());

        // Write once so the target exists with known-good content.
        ws.write("data.txt", "original content").await.unwrap();

        // A real crash mid-write can't be simulated directly, but atomicity
        // is exactly the property that a temp-file-then-rename gives us: the
        // target path only ever shows the fully-old or fully-new content,
        // never a partial write. Confirm the write leaves no stray temp
        // file behind in the target directory once it completes.
        ws.write("data.txt", "replacement content").await.unwrap();
        assert_eq!(ws.read("data.txt").await.unwrap(), "replacement content");

        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(
            names, vec!["data.txt"],
            "no leftover temp file should remain after a successful atomic write"
        );
    }

    #[tokio::test]
    async fn fs_workspace_read_caps_size_and_marks_truncation() {
        let dir = tempdir().unwrap();
        let oversized = "a".repeat(MAX_READ_BYTES as usize + 1024);
        fs::write(dir.path().join("huge.txt"), &oversized)
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let contents = ws.read("huge.txt").await.unwrap();

        assert!(
            contents.len() <= MAX_READ_BYTES as usize + "\n... (truncated, exceeds read size limit)".len(),
            "read result must not exceed the cap plus the truncation marker, got {} bytes",
            contents.len()
        );
        assert!(
            contents.ends_with("... (truncated, exceeds read size limit)"),
            "truncated read must carry an explicit marker"
        );
    }

    #[tokio::test]
    async fn fs_workspace_read_under_cap_is_not_marked_truncated() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("small.txt"), "well under the cap")
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let contents = ws.read("small.txt").await.unwrap();
        assert_eq!(contents, "well under the cap");
    }

    #[tokio::test]
    async fn fs_workspace_search_stops_at_the_match_cap_and_reports_truncation() {
        let dir = tempdir().unwrap();
        // One file with far more matching lines than MAX_SEARCH_MATCHES, so
        // traversal must stop mid-file rather than collecting them all.
        let content = "needle\n".repeat(MAX_SEARCH_MATCHES + 500);
        fs::write(dir.path().join("haystack.txt"), &content)
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = ws.search("needle").await.unwrap();

        assert!(
            result.matches.len() <= MAX_SEARCH_MATCHES,
            "search must stop collecting matches at the cap, got {}",
            result.matches.len()
        );
        assert!(
            result.truncated,
            "search must report that it stopped early rather than silently under-reporting"
        );
    }

    #[tokio::test]
    async fn fs_workspace_search_under_cap_is_not_marked_truncated() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("small.txt"), "needle once")
            .await
            .unwrap();

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = ws.search("needle").await.unwrap();
        assert_eq!(result.total_count, 1);
        assert!(!result.truncated);
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

    /// M3: a symlink pointing back at its own parent directory creates an
    /// infinite directory tree if naively followed — `sub/link -> sub`
    /// means `sub/link/link/link/...` never terminates. `search` must
    /// finish promptly (bounded by the tokio test's own timeout) rather than
    /// looping forever, and must still find the real match that exists
    /// alongside the cycle.
    #[tokio::test]
    async fn fs_workspace_search_does_not_loop_on_a_symlink_cycle() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).await.unwrap();
        fs::write(sub.join("real.rs"), "needle here").await.unwrap();
        #[cfg(unix)]
        tokio::fs::symlink(&sub, sub.join("cycle")).await.unwrap();
        #[cfg(not(unix))]
        panic!("this test only runs on unix, where tokio::fs::symlink(dir) is supported");

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), ws.search("needle"))
            .await
            .expect("search must not loop forever on a symlink cycle")
            .expect("search must succeed despite the cycle");

        assert_eq!(result.total_count, 1);
        assert_eq!(result.matches[0].file_path, PathBuf::from("sub/real.rs"));
    }

    /// M3: same cycle hazard, for `list_files` — including with
    /// `max_depth: 0` ("unlimited"), the one setting that gives the
    /// depth-based bound no chance to help at all.
    #[tokio::test]
    async fn fs_workspace_list_files_does_not_loop_on_a_symlink_cycle() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).await.unwrap();
        fs::write(sub.join("real.rs"), "content").await.unwrap();
        #[cfg(unix)]
        tokio::fs::symlink(&sub, sub.join("cycle")).await.unwrap();
        #[cfg(not(unix))]
        panic!("this test only runs on unix, where tokio::fs::symlink(dir) is supported");

        let ws = FsWorkspace::new(dir.path().to_path_buf());
        let files = tokio::time::timeout(std::time::Duration::from_secs(5), ws.list_files(0))
            .await
            .expect("list_files must not loop forever on a symlink cycle, even with max_depth: 0")
            .expect("list_files must succeed despite the cycle");

        assert!(
            files.iter().any(|f| f.path.ends_with("real.rs")),
            "the real file alongside the cycle must still be listed: {files:?}"
        );
        assert!(
            files.iter().all(|f| f.path.file_name().and_then(|n| n.to_str()) != Some("cycle")),
            "the symlink itself must not be listed either, matching this workspace's no-symlinks policy: {files:?}"
        );
    }
}

//! Case-insensitive text search over a directory tree, bounded so one call cannot turn into an
//! unbounded traversal or allocation.

use std::{future::Future, path::Path, pin::Pin};

use tokio::fs;

use crate::{
    error::WorkspaceError,
    listing::{SearchMatch, SearchResult},
};

/// Files a single search may open before it stops and reports `truncated`.
const MAX_SEARCH_FILES_SCANNED: usize = 20_000;

/// Matches a single search may collect before it stops and reports `truncated`.
pub(super) const MAX_SEARCH_MATCHES: usize = 5_000;

/// Files with these extensions are searched; a file with any other extension is skipped.
const TEXT_EXTENSIONS: [&str; 22] = [
    "txt", "rs", "toml", "yaml", "yml", "json", "md", "sh", "py", "js", "ts", "lock", "cfg", "ini",
    "conf", "env", "log", "csv", "html", "css", "xml", "svg",
];

type WalkFuture<'a> = Pin<Box<dyn Future<Output = Result<StopReason, WorkspaceError>> + Send + 'a>>;

/// Searches every text file under `root` for `query`, matches sorted by file and line.
pub(super) async fn search_tree(root: &Path, query: &str) -> Result<SearchResult, WorkspaceError> {
    let mut search = TreeSearch::new(root, query);
    let stop = search.walk(root).await?;
    Ok(search.into_result(stop))
}

/// Whether a walk covered the whole tree or stopped at a limit.
#[derive(PartialEq, Eq)]
enum StopReason {
    Finished,
    LimitReached,
}

struct TreeSearch<'a> {
    root: &'a Path,
    lowercase_query: String,
    matches: Vec<SearchMatch>,
    files_scanned: usize,
}

impl<'a> TreeSearch<'a> {
    fn new(root: &'a Path, query: &str) -> Self {
        Self {
            root,
            lowercase_query: query.to_lowercase(),
            matches: Vec::new(),
            files_scanned: 0,
        }
    }

    /// Symlinked directories are never entered: a link back to an ancestor would loop forever,
    /// and directory descents are not counted against the file limit.
    fn walk<'b>(&'b mut self, directory: &'b Path) -> WalkFuture<'b> {
        Box::pin(async move {
            let mut entries = fs::read_dir(directory).await?;
            while let Some(entry) = entries.next_entry().await? {
                if self.limit_reached() {
                    return Ok(StopReason::LimitReached);
                }
                if entry.file_type().await?.is_symlink() {
                    continue;
                }
                let path = entry.path();
                let metadata = entry.metadata().await?;
                let stop = if metadata.is_dir() {
                    self.walk(&path).await?
                } else if metadata.is_file() {
                    self.search_file(&path).await
                } else {
                    StopReason::Finished
                };
                if stop == StopReason::LimitReached {
                    return Ok(stop);
                }
            }
            Ok(StopReason::Finished)
        })
    }

    async fn search_file(&mut self, path: &Path) -> StopReason {
        self.files_scanned += 1;
        if !is_searchable_text(path) {
            return StopReason::Finished;
        }
        let Ok(contents) = fs::read_to_string(path).await else {
            return StopReason::Finished;
        };
        for (index, line) in contents.lines().enumerate() {
            if line.to_lowercase().contains(&self.lowercase_query) {
                self.matches.push(self.match_at(path, index, line));
                if self.matches.len() >= MAX_SEARCH_MATCHES {
                    return StopReason::LimitReached;
                }
            }
        }
        StopReason::Finished
    }

    fn match_at(&self, path: &Path, line_index: usize, line: &str) -> SearchMatch {
        SearchMatch {
            file_path: path.strip_prefix(self.root).unwrap_or(path).to_path_buf(),
            line_number: line_index + 1,
            line_content: line.to_owned(),
        }
    }

    fn limit_reached(&self) -> bool {
        self.files_scanned >= MAX_SEARCH_FILES_SCANNED || self.matches.len() >= MAX_SEARCH_MATCHES
    }

    fn into_result(mut self, stop: StopReason) -> SearchResult {
        self.matches.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.line_number.cmp(&b.line_number))
        });
        SearchResult {
            total_count: self.matches.len(),
            matches: self.matches,
            truncated: stop == StopReason::LimitReached,
        }
    }
}

fn is_searchable_text(path: &Path) -> bool {
    match path.extension() {
        Some(extension) => TEXT_EXTENSIONS.contains(&extension.to_string_lossy().as_ref()),
        None => true,
    }
}

//! The filters and limits both git diff tools accept, and the pathspecs they become.

use schemars::JsonSchema;
use serde::Deserialize;

/// Hard cap on patch text, whatever is requested.
pub(crate) const MAX_DIFF_BYTES: usize = 50_000;
pub(super) const DEFAULT_MAX_FILES: usize = 50;
/// Hard cap on files whose patch is included.
pub(super) const MAX_FILES_LIMIT: usize = 200;
/// Per-file summaries listed; beyond this only `total_files` counts them.
pub(super) const MAX_SUMMARY_FILES: usize = 500;
/// A file cut off by the byte budget is still included when at least this
/// much of it fits; below that it is omitted rather than shown as a stub.
pub(super) const MIN_PARTIAL_FILE_BYTES: usize = 200;
pub(super) const TRUNCATION_MARKER: &str = "\n... (diff truncated)";

/// Filters and limits accepted by both git diff tools.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct DiffFilters {
    /// Pathspecs limiting the diff, relative to the repo root (e.g. `src/`, `*.rs`).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Keep only hunks with an added or removed line containing this text (case-sensitive).
    #[serde(default)]
    pub hunk_contains: Option<String>,
    /// Maximum hunks shown per file, after `hunk_contains` filtering.
    #[serde(default)]
    pub max_hunks_per_file: Option<u32>,
    /// Maximum files whose patch is included (default 50, hard cap 200).
    #[serde(default)]
    pub max_files: Option<u32>,
    /// Maximum patch size in bytes (default and hard cap 50000).
    #[serde(default)]
    pub max_bytes: Option<u32>,
    /// Return only per-file change counts, without patch text.
    #[serde(default)]
    pub summary_only: bool,
}

impl DiffFilters {
    pub(super) fn max_files(&self) -> usize {
        self.max_files.map_or(DEFAULT_MAX_FILES, |n| {
            (n as usize).clamp(1, MAX_FILES_LIMIT)
        })
    }

    pub(super) fn max_bytes(&self) -> usize {
        self.max_bytes
            .map_or(MAX_DIFF_BYTES, |n| (n as usize).clamp(1, MAX_DIFF_BYTES))
    }

    pub(super) fn max_hunks(&self) -> usize {
        self.max_hunks_per_file
            .map_or(usize::MAX, |n| (n as usize).max(1))
    }
}

/// Adds every non-empty pathspec to `options`.
pub(crate) fn apply_pathspecs<'a>(
    options: &mut git2::DiffOptions,
    paths: impl IntoIterator<Item = &'a str>,
) {
    for path in paths
        .into_iter()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        options.pathspec(path);
    }
}

/// `String::truncate` panics inside a multi-byte character; this backs off
/// to the nearest boundary instead.
pub(crate) fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

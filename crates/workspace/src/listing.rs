//! What a workspace reports back from a search or a listing.

use std::path::PathBuf;

use serde::Serialize;

/// Represents a single search match.
#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub file_path: PathBuf,
    pub line_number: usize,
    pub line_content: String,
}

/// Result of a workspace search operation.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub total_count: usize,
    /// M3: `true` when traversal was stopped early (files-scanned or
    /// matches-collected cap reached) rather than having examined the whole
    /// workspace tree — callers must not treat `matches` as exhaustive.
    #[serde(default)]
    pub truncated: bool,
}

/// Metadata about a file in the workspace.
#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub is_directory: bool,
}

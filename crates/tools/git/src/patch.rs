//! Deterministic limits shared by `git.diff` and `git.show`: pathspec and
//! hunk filters, file/hunk/byte caps, and a summary-only mode. Nothing here
//! rewrites patch content -- lines are either included exactly as git2
//! produced them or left out, and every omission is reported in the result.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Hard cap on patch text, whatever is requested.
pub(crate) const MAX_DIFF_BYTES: usize = 50_000;
const DEFAULT_MAX_FILES: usize = 50;
/// Hard cap on files whose patch is included.
const MAX_FILES_LIMIT: usize = 200;
/// Per-file summaries listed; beyond this only `total_files` counts them.
const MAX_SUMMARY_FILES: usize = 500;
/// A file cut off by the byte budget is still included when at least this
/// much of it fits; below that it is omitted rather than shown as a stub.
const MIN_PARTIAL_FILE_BYTES: usize = 200;
const TRUNCATION_MARKER: &str = "\n... (diff truncated)";

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
    fn max_files(&self) -> usize {
        self.max_files.map_or(DEFAULT_MAX_FILES, |n| {
            (n as usize).clamp(1, MAX_FILES_LIMIT)
        })
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
            .map_or(MAX_DIFF_BYTES, |n| (n as usize).clamp(1, MAX_DIFF_BYTES))
    }

    fn max_hunks(&self) -> usize {
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

/// How much of one file's patch made it into the result.
#[derive(Clone, Copy)]
enum PatchState {
    Full,
    Partial,
    /// Cut by `max_files` or the byte budget.
    Omitted,
    /// No hunk matched `hunk_contains`.
    FilteredOut,
    /// `summary_only`, or the caller asked for no diff.
    Summary,
}

impl PatchState {
    fn as_str(self) -> &'static str {
        match self {
            PatchState::Full => "full",
            PatchState::Partial => "partial",
            PatchState::Omitted => "omitted",
            PatchState::FilteredOut => "filtered_out",
            PatchState::Summary => "summary",
        }
    }
}

/// Renders `diff` under `filters`, returning the fields both tools add to
/// their output: `diff`, `files`, `total_files`, and `limits` (only when
/// something was left out).
pub(crate) fn render(
    diff: &git2::Diff,
    filters: &DiffFilters,
    include_patch: bool,
) -> Result<Map<String, Value>, String> {
    let include_patch = include_patch && !filters.summary_only;
    let (max_files, max_bytes, max_hunks) = (
        filters.max_files(),
        filters.max_bytes(),
        filters.max_hunks(),
    );
    let needle = filters
        .hunk_contains
        .as_deref()
        .filter(|needle| !needle.is_empty());

    let mut text = String::new();
    let mut files = Vec::new();
    let mut patched_files = 0usize;
    let mut omitted_files = 0usize;
    let mut omitted_hunks = 0usize;
    let mut bytes_truncated = false;
    let total_files = diff.deltas().len();

    for (index, delta) in diff.deltas().enumerate() {
        let old_path = delta
            .old_file()
            .path()
            .map(|p| p.to_string_lossy().into_owned());
        let new_path = delta
            .new_file()
            .path()
            .map(|p| p.to_string_lossy().into_owned());
        let path = new_path
            .clone()
            .or_else(|| old_path.clone())
            .unwrap_or_default();
        let patch = git2::Patch::from_diff(diff, index).map_err(|e| e.to_string())?;
        let binary = patch.is_none() || delta.flags().is_binary();
        let (additions, deletions, hunks_total) = match &patch {
            Some(patch) => {
                let (_, additions, deletions) = patch.line_stats().map_err(|e| e.to_string())?;
                (additions, deletions, patch.num_hunks())
            }
            None => (0, 0, 0),
        };

        let mut state = PatchState::Summary;
        let mut hunks_shown = None;
        if include_patch {
            if patched_files >= max_files || bytes_truncated {
                state = PatchState::Omitted;
                omitted_files += 1;
            } else {
                let block = file_block(&delta, patch.as_ref(), needle, max_hunks)?;
                omitted_hunks += block.omitted_hunks;
                hunks_shown = Some(block.hunks_shown);
                match block.text {
                    None => state = PatchState::FilteredOut,
                    Some(block_text) => {
                        let remaining = max_bytes.saturating_sub(text.len());
                        if block_text.len() <= remaining {
                            text.push_str(&block_text);
                            state = PatchState::Full;
                            patched_files += 1;
                        } else {
                            bytes_truncated = true;
                            let room = remaining.saturating_sub(TRUNCATION_MARKER.len());
                            if room >= MIN_PARTIAL_FILE_BYTES {
                                text.push_str(truncate_at_char_boundary(&block_text, room));
                                text.push_str(TRUNCATION_MARKER);
                                state = PatchState::Partial;
                                patched_files += 1;
                            } else {
                                state = PatchState::Omitted;
                                omitted_files += 1;
                            }
                        }
                    }
                }
            }
        }

        if files.len() < MAX_SUMMARY_FILES {
            let mut summary = json!({
                "path": path,
                "status": format!("{:?}", delta.status()).to_lowercase(),
                "additions": additions,
                "deletions": deletions,
                "hunks": hunks_total,
                "patch": state.as_str(),
            });
            if old_path.is_some() && old_path != new_path {
                summary["old_path"] = json!(old_path);
            }
            if binary {
                summary["binary"] = json!(true);
            }
            if let Some(shown) = hunks_shown.filter(|shown| *shown != hunks_total) {
                summary["hunks_shown"] = json!(shown);
            }
            files.push(summary);
        }
    }

    let mut fields = Map::new();
    fields.insert("diff".into(), json!(text));
    fields.insert("total_files".into(), json!(total_files));
    fields.insert("files".into(), json!(files));
    let mut limits = Map::new();
    if bytes_truncated {
        limits.insert("max_bytes".into(), json!(max_bytes));
    }
    if omitted_files > 0 {
        limits.insert("omitted_files".into(), json!(omitted_files));
    }
    if omitted_hunks > 0 {
        limits.insert("omitted_hunks".into(), json!(omitted_hunks));
    }
    if files.len() < total_files {
        limits.insert("unlisted_files".into(), json!(total_files - files.len()));
    }
    if !limits.is_empty() {
        limits.insert(
            "hint".into(),
            json!("Narrow with paths, hunk_contains, or max_hunks_per_file, or use summary_only to see every file."),
        );
        fields.insert("limits".into(), Value::Object(limits));
    }
    Ok(fields)
}

struct FileBlock {
    /// `None` when every hunk was filtered out by `hunk_contains`.
    text: Option<String>,
    hunks_shown: usize,
    omitted_hunks: usize,
}

fn file_block(
    delta: &git2::DiffDelta,
    patch: Option<&git2::Patch>,
    needle: Option<&str>,
    max_hunks: usize,
) -> Result<FileBlock, String> {
    let old = delta
        .old_file()
        .path()
        .map(|p| format!("a/{}", p.to_string_lossy()));
    let new = delta
        .new_file()
        .path()
        .map(|p| format!("b/{}", p.to_string_lossy()));
    let mut text = format!(
        "diff --git {} {}\n",
        old.as_deref().or(new.as_deref()).unwrap_or("a/?"),
        new.as_deref().or(old.as_deref()).unwrap_or("b/?"),
    );
    let Some(patch) = patch else {
        if needle.is_some() {
            return Ok(FileBlock {
                text: None,
                hunks_shown: 0,
                omitted_hunks: 0,
            });
        }
        text.push_str("Binary files differ\n");
        return Ok(FileBlock {
            text: Some(text),
            hunks_shown: 0,
            omitted_hunks: 0,
        });
    };
    let dev_null = |present: bool, name: &Option<String>| {
        if present {
            name.clone().unwrap_or_default()
        } else {
            "/dev/null".to_string()
        }
    };
    text.push_str(&format!(
        "--- {}\n",
        dev_null(delta.status() != git2::Delta::Added, &old)
    ));
    text.push_str(&format!(
        "+++ {}\n",
        dev_null(delta.status() != git2::Delta::Deleted, &new)
    ));

    let mut shown = 0usize;
    let mut omitted = 0usize;
    for hunk_index in 0..patch.num_hunks() {
        let (hunk, line_count) = patch.hunk(hunk_index).map_err(|e| e.to_string())?;
        let mut hunk_text = String::from_utf8_lossy(hunk.header()).into_owned();
        let mut matches = needle.is_none();
        for line_index in 0..line_count {
            let line = patch
                .line_in_hunk(hunk_index, line_index)
                .map_err(|e| e.to_string())?;
            let content = String::from_utf8_lossy(line.content());
            if let Some(needle) = needle {
                matches |= matches!(line.origin(), '+' | '-') && content.contains(needle);
            }
            if matches!(line.origin(), '+' | '-' | ' ') {
                hunk_text.push(line.origin());
            }
            hunk_text.push_str(&content);
        }
        if !matches {
            continue;
        }
        if shown >= max_hunks {
            omitted += 1;
            continue;
        }
        text.push_str(&hunk_text);
        shown += 1;
    }
    if needle.is_some() && shown == 0 {
        return Ok(FileBlock {
            text: None,
            hunks_shown: 0,
            omitted_hunks: omitted,
        });
    }
    if omitted > 0 {
        text.push_str(&format!("... ({omitted} more matching hunk(s) omitted)\n"));
    }
    Ok(FileBlock {
        text: Some(text),
        hunks_shown: shown,
        omitted_hunks: omitted,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "ab€cd"; // '€' is 3 bytes, occupying bytes 2..5
        assert_eq!(truncate_at_char_boundary(text, 3), "ab");
        assert_eq!(truncate_at_char_boundary(text, 5), "ab€");
        assert_eq!(truncate_at_char_boundary(text, 50), text);
    }

    #[test]
    fn requested_limits_are_clamped_to_the_hard_caps() {
        let filters = DiffFilters {
            max_files: Some(10_000),
            max_bytes: Some(10_000_000),
            max_hunks_per_file: Some(0),
            ..Default::default()
        };
        assert_eq!(filters.max_files(), MAX_FILES_LIMIT);
        assert_eq!(filters.max_bytes(), MAX_DIFF_BYTES);
        assert_eq!(filters.max_hunks(), 1);
        assert_eq!(DiffFilters::default().max_files(), DEFAULT_MAX_FILES);
    }
}

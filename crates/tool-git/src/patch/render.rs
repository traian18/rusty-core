//! Rendering a whole diff under the filters, one file at a time.

use serde_json::{json, Map, Value};

use super::{
    file_block::file_block,
    filters::{
        truncate_at_char_boundary, DiffFilters, MAX_SUMMARY_FILES, MIN_PARTIAL_FILE_BYTES,
        TRUNCATION_MARKER,
    },
    summary::{file_summary, FileFacts, Omissions, PatchState},
};

/// Renders `diff` under `filters`, returning the fields both tools add to their output: `diff`,
/// `files`, `total_files`, and `limits` (only when something was left out).
pub(crate) fn render(
    diff: &git2::Diff,
    filters: &DiffFilters,
    include_patch: bool,
) -> Result<Map<String, Value>, String> {
    let mut run = Render::new(filters, include_patch && !filters.summary_only);
    for (index, delta) in diff.deltas().enumerate() {
        run.add_file(diff, index, &delta)?;
    }
    Ok(run.into_fields(diff.deltas().len()))
}

/// The result being built, and the budgets it is built under.
struct Render<'a> {
    include_patch: bool,
    max_files: usize,
    max_bytes: usize,
    max_hunks: usize,
    needle: Option<&'a str>,
    text: String,
    files: Vec<Value>,
    patched_files: usize,
    omitted_files: usize,
    omitted_hunks: usize,
    bytes_truncated: bool,
}

impl<'a> Render<'a> {
    fn new(filters: &'a DiffFilters, include_patch: bool) -> Self {
        Self {
            include_patch,
            max_files: filters.max_files(),
            max_bytes: filters.max_bytes(),
            max_hunks: filters.max_hunks(),
            needle: filters
                .hunk_contains
                .as_deref()
                .filter(|needle| !needle.is_empty()),
            text: String::new(),
            files: Vec::new(),
            patched_files: 0,
            omitted_files: 0,
            omitted_hunks: 0,
            bytes_truncated: false,
        }
    }

    fn add_file(
        &mut self,
        diff: &git2::Diff,
        index: usize,
        delta: &git2::DiffDelta,
    ) -> Result<(), String> {
        let patch = git2::Patch::from_diff(diff, index).map_err(|e| e.to_string())?;
        let facts = FileFacts::of(delta, patch.as_ref())?;
        let (state, hunks_shown) = if self.include_patch {
            self.place_patch(delta, patch.as_ref())?
        } else {
            (PatchState::Summary, None)
        };
        if self.files.len() < MAX_SUMMARY_FILES {
            self.files
                .push(file_summary(delta, &facts, state, hunks_shown));
        }
        Ok(())
    }

    /// Adds the file's patch text if the file and byte budgets allow, and says what happened.
    fn place_patch(
        &mut self,
        delta: &git2::DiffDelta,
        patch: Option<&git2::Patch>,
    ) -> Result<(PatchState, Option<usize>), String> {
        if self.patched_files >= self.max_files || self.bytes_truncated {
            self.omitted_files += 1;
            return Ok((PatchState::Omitted, None));
        }
        let block = file_block(delta, patch, self.needle, self.max_hunks)?;
        self.omitted_hunks += block.omitted_hunks;
        let state = match block.text {
            None => PatchState::FilteredOut,
            Some(text) => self.append_within_budget(&text),
        };
        Ok((state, Some(block.hunks_shown)))
    }

    /// Appends the whole block if it fits; otherwise as much of it as leaves room for the
    /// truncation marker, provided that is worth showing; otherwise none of it.
    fn append_within_budget(&mut self, block: &str) -> PatchState {
        let remaining = self.max_bytes.saturating_sub(self.text.len());
        if block.len() <= remaining {
            self.text.push_str(block);
            self.patched_files += 1;
            return PatchState::Full;
        }
        self.bytes_truncated = true;
        let room = remaining.saturating_sub(TRUNCATION_MARKER.len());
        if room < MIN_PARTIAL_FILE_BYTES {
            self.omitted_files += 1;
            return PatchState::Omitted;
        }
        self.text.push_str(truncate_at_char_boundary(block, room));
        self.text.push_str(TRUNCATION_MARKER);
        self.patched_files += 1;
        PatchState::Partial
    }

    fn into_fields(self, total_files: usize) -> Map<String, Value> {
        let omissions = Omissions {
            max_bytes: self.bytes_truncated.then_some(self.max_bytes),
            omitted_files: self.omitted_files,
            omitted_hunks: self.omitted_hunks,
            unlisted_files: total_files.saturating_sub(self.files.len()),
        };
        let mut fields = Map::new();
        fields.insert("diff".into(), json!(self.text));
        fields.insert("total_files".into(), json!(total_files));
        fields.insert("files".into(), json!(self.files));
        if let Some(limits) = omissions.into_limits() {
            fields.insert("limits".into(), limits);
        }
        fields
    }
}

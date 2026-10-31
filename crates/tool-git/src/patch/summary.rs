//! The per-file summary and the report of what was left out.

use serde_json::{json, Map, Value};

/// How much of one file's patch made it into the result.
#[derive(Clone, Copy)]
pub(super) enum PatchState {
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

/// A file's paths and change counts.
pub(super) struct FileFacts {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    pub additions: usize,
    pub deletions: usize,
    pub hunks_total: usize,
}

impl FileFacts {
    pub(super) fn of(delta: &git2::DiffDelta, patch: Option<&git2::Patch>) -> Result<Self, String> {
        let path_of =
            |file: git2::DiffFile<'_>| file.path().map(|path| path.to_string_lossy().into_owned());
        let (additions, deletions, hunks_total) = match patch {
            Some(patch) => {
                let (_, additions, deletions) = patch.line_stats().map_err(|e| e.to_string())?;
                (additions, deletions, patch.num_hunks())
            }
            None => (0, 0, 0),
        };
        Ok(Self {
            old_path: path_of(delta.old_file()),
            new_path: path_of(delta.new_file()),
            binary: patch.is_none() || delta.flags().is_binary(),
            additions,
            deletions,
            hunks_total,
        })
    }

    fn path(&self) -> String {
        self.new_path
            .clone()
            .or_else(|| self.old_path.clone())
            .unwrap_or_default()
    }
}

/// One entry of the `files` list.
pub(super) fn file_summary(
    delta: &git2::DiffDelta,
    facts: &FileFacts,
    state: PatchState,
    hunks_shown: Option<usize>,
) -> Value {
    let mut summary = json!({
        "path": facts.path(),
        "status": format!("{:?}", delta.status()).to_lowercase(),
        "additions": facts.additions,
        "deletions": facts.deletions,
        "hunks": facts.hunks_total,
        "patch": state.as_str(),
    });
    if facts.old_path.is_some() && facts.old_path != facts.new_path {
        summary["old_path"] = json!(facts.old_path);
    }
    if facts.binary {
        summary["binary"] = json!(true);
    }
    if let Some(shown) = hunks_shown.filter(|shown| *shown != facts.hunks_total) {
        summary["hunks_shown"] = json!(shown);
    }
    summary
}

/// What was left out, if anything, with a hint on how to see more.
pub(super) struct Omissions {
    pub max_bytes: Option<usize>,
    pub omitted_files: usize,
    pub omitted_hunks: usize,
    pub unlisted_files: usize,
}

impl Omissions {
    pub(super) fn into_limits(self) -> Option<Value> {
        let mut limits = Map::new();
        if let Some(max_bytes) = self.max_bytes {
            limits.insert("max_bytes".into(), json!(max_bytes));
        }
        if self.omitted_files > 0 {
            limits.insert("omitted_files".into(), json!(self.omitted_files));
        }
        if self.omitted_hunks > 0 {
            limits.insert("omitted_hunks".into(), json!(self.omitted_hunks));
        }
        if self.unlisted_files > 0 {
            limits.insert("unlisted_files".into(), json!(self.unlisted_files));
        }
        if limits.is_empty() {
            return None;
        }
        limits.insert(
            "hint".into(),
            json!("Narrow with paths, hunk_contains, or max_hunks_per_file, or use summary_only to see every file."),
        );
        Some(Value::Object(limits))
    }
}

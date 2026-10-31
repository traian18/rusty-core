//! One file's patch text: its header, then the hunks that pass the filters.

/// One file's rendered patch and how many of its hunks it shows.
pub(super) struct FileBlock {
    /// `None` when every hunk was filtered out by `hunk_contains`.
    pub text: Option<String>,
    pub hunks_shown: usize,
    pub omitted_hunks: usize,
}

impl FileBlock {
    fn filtered_out(omitted_hunks: usize) -> Self {
        Self {
            text: None,
            hunks_shown: 0,
            omitted_hunks,
        }
    }
}

/// The selected hunks of a patch.
struct Hunks {
    text: String,
    shown: usize,
    omitted: usize,
}

pub(super) fn file_block(
    delta: &git2::DiffDelta,
    patch: Option<&git2::Patch>,
    needle: Option<&str>,
    max_hunks: usize,
) -> Result<FileBlock, String> {
    let mut text = diff_header(delta);
    let Some(patch) = patch else {
        return Ok(binary_block(text, needle));
    };
    text.push_str(&file_markers(delta));
    let hunks = select_hunks(patch, needle, max_hunks)?;
    if needle.is_some() && hunks.shown == 0 {
        return Ok(FileBlock::filtered_out(hunks.omitted));
    }
    text.push_str(&hunks.text);
    if hunks.omitted > 0 {
        text.push_str(&format!(
            "... ({} more matching hunk(s) omitted)\n",
            hunks.omitted
        ));
    }
    Ok(FileBlock {
        text: Some(text),
        hunks_shown: hunks.shown,
        omitted_hunks: hunks.omitted,
    })
}

/// A file with no text patch: shown as "Binary files differ", unless a hunk filter is active (no
/// hunk can match then).
fn binary_block(mut text: String, needle: Option<&str>) -> FileBlock {
    if needle.is_some() {
        return FileBlock::filtered_out(0);
    }
    text.push_str("Binary files differ\n");
    FileBlock {
        text: Some(text),
        hunks_shown: 0,
        omitted_hunks: 0,
    }
}

fn diff_header(delta: &git2::DiffDelta) -> String {
    let old = delta
        .old_file()
        .path()
        .map(|p| format!("a/{}", p.to_string_lossy()));
    let new = delta
        .new_file()
        .path()
        .map(|p| format!("b/{}", p.to_string_lossy()));
    format!(
        "diff --git {} {}\n",
        old.as_deref().or(new.as_deref()).unwrap_or("a/?"),
        new.as_deref().or(old.as_deref()).unwrap_or("b/?"),
    )
}

/// The `---` and `+++` lines; an added file has no old side and a deleted one no new side.
fn file_markers(delta: &git2::DiffDelta) -> String {
    let name = |file: git2::DiffFile<'_>, prefix: &str, present: bool| {
        if !present {
            return "/dev/null".to_string();
        }
        file.path()
            .map(|path| format!("{prefix}/{}", path.to_string_lossy()))
            .unwrap_or_default()
    };
    format!(
        "--- {}\n+++ {}\n",
        name(delta.old_file(), "a", delta.status() != git2::Delta::Added),
        name(
            delta.new_file(),
            "b",
            delta.status() != git2::Delta::Deleted
        ),
    )
}

/// The hunks that contain the needle (all when there is none), up to `max_hunks`.
fn select_hunks(
    patch: &git2::Patch,
    needle: Option<&str>,
    max_hunks: usize,
) -> Result<Hunks, String> {
    let mut hunks = Hunks {
        text: String::new(),
        shown: 0,
        omitted: 0,
    };
    for hunk_index in 0..patch.num_hunks() {
        let (hunk_text, matches) = hunk_text(patch, hunk_index, needle)?;
        if !matches {
            continue;
        }
        if hunks.shown >= max_hunks {
            hunks.omitted += 1;
            continue;
        }
        hunks.text.push_str(&hunk_text);
        hunks.shown += 1;
    }
    Ok(hunks)
}

/// One hunk's text, and whether it matches: always, with no needle; otherwise when an added or
/// removed line contains it.
fn hunk_text(
    patch: &git2::Patch,
    hunk_index: usize,
    needle: Option<&str>,
) -> Result<(String, bool), String> {
    let (hunk, line_count) = patch.hunk(hunk_index).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(hunk.header()).into_owned();
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
            text.push(line.origin());
        }
        text.push_str(&content);
    }
    Ok((text, matches))
}

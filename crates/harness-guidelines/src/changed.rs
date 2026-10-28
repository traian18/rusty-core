//! Holding a change to the guidelines without holding it to what was there before: only what the
//! change introduced counts, so an already untidy repository does not block every edit.
use crate::check::Violation;

/// What changed in one file, from a diff against the last commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedLines {
    /// Added or modified lines of the new file, 1-based and inclusive.
    pub ranges: Vec<(u32, u32)>,
    /// Lines the file had before; `None` for a new file.
    pub base_line_count: Option<u32>,
}

impl ChangedLines {
    /// A file that did not exist before: every line is new.
    pub fn new_file() -> Self {
        Self::default()
    }

    fn touches(&self, start: u32, end: u32) -> bool {
        self.base_line_count.is_none() || self.ranges.iter().any(|(a, b)| *a <= end && start <= *b)
    }
}

/// The violations `changed` introduced: those on changed lines (a function's whole span counts),
/// and a file-wide limit only when the file is new or grew while over it.
pub fn retain_introduced(violations: Vec<Violation>, changed: &ChangedLines) -> Vec<Violation> {
    violations
        .into_iter()
        .filter(|v| {
            if v.file_wide() {
                return match (changed.base_line_count, v.measured) {
                    (None, _) => true,
                    (Some(before), Some(now)) => now > before && !changed.ranges.is_empty(),
                    (Some(_), None) => false,
                };
            }
            changed.touches(v.start_line, v.end_line)
        })
        .collect()
}

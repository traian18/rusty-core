//! Reading source text to turn byte spans into line and column.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use harness_workspace::repository_intelligence::{read_contained_file, RootRelativePath};

use crate::model::{Location, SourceSpan};

/// Supplies file contents; the filesystem implementation keeps the same contained-read
/// guarantees as discovery, and tests can substitute an in-memory one.
pub trait SourceTextReader: Send + Sync {
    fn read(&self, path: &RootRelativePath, max_bytes: u64) -> std::io::Result<Vec<u8>>;
}

/// Reads from an authorized root without following symlinks.
pub struct FsSourceTextReader {
    root: PathBuf,
}

impl FsSourceTextReader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl SourceTextReader for FsSourceTextReader {
    fn read(&self, path: &RootRelativePath, max_bytes: u64) -> std::io::Result<Vec<u8>> {
        let root: &Path = &self.root;
        read_contained_file(&std::fs::canonicalize(root)?, path.as_path(), max_bytes)
    }
}

/// Byte offsets of line starts for one file.
struct LineIndex {
    starts: Vec<usize>,
    text: String,
}

impl LineIndex {
    fn new(text: String) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
        Self { starts, text }
    }

    /// 1-based line and column (in characters) of `offset`, or `None` outside the text.
    fn position(&self, offset: usize) -> Option<(u32, u32)> {
        if offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return None;
        }
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let column = self.text[self.starts[line]..offset].chars().count();
        Some((line as u32 + 1, column as u32 + 1))
    }
}

/// What is known about a file: its text, or why only byte spans can be trusted.
enum FileState {
    Matching(LineIndex),
    Stale,
}

/// Resolves spans to locations, reading each file at most once.
pub struct Locator<'a> {
    reader: &'a dyn SourceTextReader,
    max_bytes: u64,
    /// Content hash recorded in the snapshot for each indexed file.
    expected_hashes: &'a HashMap<&'a RootRelativePath, &'a str>,
    files: HashMap<RootRelativePath, FileState>,
}

impl<'a> Locator<'a> {
    pub fn new(
        reader: &'a dyn SourceTextReader,
        max_bytes: u64,
        expected_hashes: &'a HashMap<&'a RootRelativePath, &'a str>,
    ) -> Self {
        Self {
            reader,
            max_bytes,
            expected_hashes,
            files: HashMap::new(),
        }
    }

    pub fn locate(&mut self, span: &SourceSpan) -> Location {
        let state = self.state_of(&span.file);
        let position = match state {
            FileState::Matching(index) => index.position(span.start),
            FileState::Stale => None,
        };
        Location {
            file: span.file.clone(),
            start: span.start,
            end: span.end,
            line: position.map(|(line, _)| line),
            column: position.map(|(_, column)| column),
            stale: matches!(state, FileState::Stale),
        }
    }

    fn state_of(&mut self, file: &RootRelativePath) -> &FileState {
        if !self.files.contains_key(file) {
            let state = self.load(file);
            self.files.insert(file.clone(), state);
        }
        &self.files[file]
    }

    /// Text that is readable and still matches the snapshot's hash; anything else is stale.
    fn load(&self, file: &RootRelativePath) -> FileState {
        let Ok(bytes) = self.reader.read(file, self.max_bytes) else {
            return FileState::Stale;
        };
        if let Some(expected) = self.expected_hashes.get(file) {
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if actual != *expected {
                return FileState::Stale;
            }
        }
        match String::from_utf8(bytes) {
            Ok(text) => FileState::Matching(LineIndex::new(text)),
            Err(_) => FileState::Stale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_lines_and_characters() {
        let index = LineIndex::new("ab\ncé d\n\nlast".to_owned());
        assert_eq!(index.position(0), Some((1, 1)));
        assert_eq!(index.position(1), Some((1, 2)));
        assert_eq!(index.position(3), Some((2, 1)));
        // "é" is two bytes but one character: the space after it is column 3.
        assert_eq!(index.position(6), Some((2, 3)));
        assert_eq!(index.position(9), Some((3, 1)));
        assert_eq!(index.position(10), Some((4, 1)));
        // Inside a multibyte character, or past the end, there is no position.
        assert_eq!(index.position(5), None);
        assert_eq!(index.position(99), None);
    }
}

//! Turning byte spans into line ranges, with the content hash the snapshot recorded.
use std::{collections::HashMap, sync::Arc};

use harness_workspace::{
    import_graph::ByteSpan,
    repository_intelligence::{RepositoryGraph, RootRelativePath},
};

use crate::{facts::path_text, model::Citation};

/// Largest file read to find line numbers.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// Byte offsets of line starts for one file.
pub struct LineMap {
    starts: Vec<usize>,
}

impl LineMap {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
        Self { starts }
    }

    /// 1-based line of a byte offset.
    pub fn line_of(&self, offset: usize) -> u32 {
        match self.starts.binary_search(&offset) {
            Ok(index) => index as u32 + 1,
            Err(index) => index as u32,
        }
    }

    pub fn lines(&self, span: ByteSpan) -> (u32, u32) {
        let start = self.line_of(span.start);
        let end = self.line_of(span.end.saturating_sub(1).max(span.start));
        (start, end.max(start))
    }
}

/// Reads files through the graph and remembers their line maps for one page.
pub struct Sources {
    graph: Arc<dyn RepositoryGraph>,
    maps: HashMap<RootRelativePath, Option<LineMap>>,
}

impl Sources {
    pub fn new(graph: Arc<dyn RepositoryGraph>) -> Self {
        Self {
            graph,
            maps: HashMap::new(),
        }
    }

    fn map(&mut self, file: &RootRelativePath) -> Option<&LineMap> {
        if !self.maps.contains_key(file) {
            let map = self
                .graph
                .read_file(file, MAX_SOURCE_BYTES)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|text| LineMap::new(&text));
            self.maps.insert(file.clone(), map);
        }
        self.maps.get(file).and_then(Option::as_ref)
    }

    /// A citation for `span` of `file`. `content_hash` is what the snapshot recorded for the file;
    /// when the file cannot be read (or changed so the span no longer fits) the lines are the
    /// first line, and the hash still says what the claim was read at.
    pub fn cite(
        &mut self,
        file: &RootRelativePath,
        span: ByteSpan,
        content_hash: &str,
    ) -> Citation {
        let (start_line, end_line) = self.map(file).map_or((1, 1), |map| map.lines(span));
        Citation {
            file: path_text(file),
            start_line,
            end_line,
            content_hash: content_hash.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_offsets_to_one_based_lines() {
        let map = LineMap::new("ab\ncd\nef");
        assert_eq!(map.line_of(0), 1);
        assert_eq!(map.line_of(2), 1);
        assert_eq!(map.line_of(3), 2);
        assert_eq!(map.line_of(7), 3);
        assert_eq!(map.lines(ByteSpan { start: 1, end: 5 }), (1, 2));
        assert_eq!(map.lines(ByteSpan { start: 3, end: 3 }), (2, 2));
    }
}

//! What a writer is shown: bounded excerpts of real files, each recorded with the content hash it
//! had, so a page can say when its inputs moved on.
use std::collections::BTreeMap;

use harness_workspace::repository_intelligence::{content_hash, RepositoryGraph, RootRelativePath};

use super::model::InputFile;

/// Largest file read.
const MAX_BYTES: u64 = 512 * 1024;

/// One excerpt: a path, a 1-based line range, and the text.
#[derive(Clone, Debug)]
pub struct Block {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub label: String,
    pub text: String,
}

pub struct Evidence<'a> {
    graph: &'a dyn RepositoryGraph,
    pub blocks: Vec<Block>,
    inputs: BTreeMap<String, String>,
    /// Every path and identifier the writer was shown, for grounding what it writes.
    pub mentioned: Vec<String>,
}

impl<'a> Evidence<'a> {
    pub fn new(graph: &'a dyn RepositoryGraph) -> Self {
        Self {
            graph,
            blocks: Vec::new(),
            inputs: BTreeMap::new(),
            mentioned: Vec::new(),
        }
    }

    /// The whole text of `path`, recorded as an input; `None` when it cannot be read.
    pub fn read(&mut self, path: &str) -> Option<String> {
        let file = RootRelativePath::new(path).ok()?;
        let bytes = self.graph.read_file(&file, MAX_BYTES).ok()?;
        self.inputs.insert(path.to_owned(), content_hash(&bytes));
        String::from_utf8(bytes).ok()
    }

    /// Lines `start..=end` of `path` as a block (at most `max_lines`).
    pub fn lines(&mut self, path: &str, start: u32, end: u32, max_lines: u32, label: &str) -> bool {
        let Some(text) = self.read(path) else {
            return false;
        };
        let all: Vec<&str> = text.lines().collect();
        if all.is_empty() {
            return false;
        }
        let first = (start.max(1) as usize).min(all.len());
        let last = (end as usize)
            .clamp(first, all.len())
            .min(first + max_lines as usize - 1);
        if self
            .blocks
            .iter()
            .any(|b| b.path == path && b.start as usize <= first && b.end as usize >= last)
        {
            return true;
        }
        self.blocks.push(Block {
            path: path.to_owned(),
            start: first as u32,
            end: last as u32,
            label: label.to_owned(),
            text: all[first - 1..last].join("\n"),
        });
        self.mention(path);
        true
    }

    /// Records a name the writer may use in backticks.
    pub fn mention(&mut self, name: &str) {
        if !name.is_empty() {
            self.mentioned.push(name.to_owned());
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            let lines = if block.start == block.end {
                format!("line {}", block.start)
            } else {
                format!("lines {}-{}", block.start, block.end)
            };
            out.push_str(&format!(
                "### `{}` ({lines}){}\n```\n{}\n```\n\n",
                block.path,
                if block.label.is_empty() {
                    String::new()
                } else {
                    format!(": {}", block.label)
                },
                block.text
            ));
        }
        out
    }

    pub fn inputs(&self) -> Vec<InputFile> {
        self.inputs
            .iter()
            .map(|(file, hash)| InputFile {
                file: file.clone(),
                content_hash: hash.clone(),
            })
            .collect()
    }
}

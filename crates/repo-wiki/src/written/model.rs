//! A written page: documentation prose a model wrote from the evidence it was given, kept with
//! what it was written from so its references can be checked, shown with the files it names, and
//! marked when the code moves on.
use serde::{Deserialize, Serialize};

use crate::model::Citation;

/// What a footnote of a written page points at.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    /// A line range of a file, read at a content hash.
    Lines { citation: Citation },
    /// Something computed from the index (for example how often one component imports another),
    /// described in words.
    Derived { description: String },
}

/// A file a page names, numbered in order of first mention.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub n: u32,
    pub reference: SourceRef,
    /// What the writer was shown for this source.
    pub excerpt: String,
}

/// A file a page was written from, at the content hash it had.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputFile {
    pub file: String,
    pub content_hash: String,
}

/// Who wrote a page, so it can be audited and rewritten.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Author {
    pub model: String,
    pub style: String,
    pub written_at_ms: u64,
    /// The commit the repository was at, when known.
    pub commit: Option<String>,
}

/// The kinds of page a developer reads, in reading order for the ones that start a reader off.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrittenKind {
    /// What the project is, how it is put together, and the ideas it works in.
    Overview,
    /// Prerequisites, install, run and test, only from what the repository documents.
    GettingStarted,
    /// The named ideas a reader has to learn, where they live and how they connect.
    KeyConcepts,
    /// One end-to-end walk through the system.
    HowItWorks,
    /// The parts, how work moves through them, and the boundaries.
    Architecture,
    /// Where work is happening now, what is stable, and what to watch out for.
    ActiveLandscape,
    /// The repository's own terms, defined in its own sentences (no model).
    Glossary,
    Module,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WrittenPage {
    /// `written:<kind>` for the pages that start a reader off, or the structural page id of a
    /// module or file.
    pub id: String,
    pub kind: WrittenKind,
    pub title: String,
    /// Markdown that names files and symbols in backticks; diagrams are `@@diagram:key@@`.
    pub markdown: String,
    /// The files the page names, in order of first mention.
    pub footnotes: Vec<Source>,
    pub diagrams: Vec<String>,
    /// Digest of the facts the page was written from.
    pub facts_digest: String,
    pub inputs: Vec<InputFile>,
    pub author: Author,
    /// Backticked paths and symbols the evidence does not contain, shown as plain text instead.
    pub dropped: usize,
    /// Too many references did not check out, or the page is too thin to trust.
    pub needs_review: bool,
}

/// How far the code has moved since a page was written.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// The facts and every input file are as they were.
    Fresh,
    /// Something changed, but most of what the page cites still checks out.
    Stale,
    /// Most of the page's inputs changed or are gone; it should be rewritten before it is read.
    Expired,
}

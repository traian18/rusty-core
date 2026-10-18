//! What a page is: sections of facts, each fact with the citations and links that support it.
use serde::{Deserialize, Serialize};

use crate::error::WikiError;

/// Levels a page can describe. A page cites the level below it (a folder links its files).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageLevel {
    Repository,
    Package,
    Folder,
    File,
}

impl PageLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Repository => "repo",
            Self::Package => "package",
            Self::Folder => "folder",
            Self::File => "file",
        }
    }
}

/// `repo:`, `package:<group id>`, `folder:<dir:path | rest:focus>`, `file:<path>`. The same
/// target ids work in `repo_context` and on the architecture canvas.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PageId(pub String);

impl PageId {
    pub fn new(level: PageLevel, target: &str) -> Self {
        Self(format!("{}:{target}", level.name()))
    }

    pub fn repository() -> Self {
        Self::new(PageLevel::Repository, "")
    }

    pub fn file(path: &str) -> Self {
        Self::new(PageLevel::File, path)
    }

    pub fn folder(id: &str) -> Self {
        Self::new(PageLevel::Folder, id)
    }

    /// The level and target this id names.
    pub fn parse(&self) -> Result<(PageLevel, &str), WikiError> {
        let text = self.0.as_str();
        let (level, target) = text.split_once(':').unwrap_or((text, ""));
        let level = match level {
            "repo" | "repository" => PageLevel::Repository,
            "package" => PageLevel::Package,
            "folder" => PageLevel::Folder,
            "file" => PageLevel::File,
            _ => return Err(WikiError::InvalidId(text.to_owned())),
        };
        Ok((level, target))
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A line range of a file, read at a known content hash. A citation is checkable: the range is
/// still right only while the file's current hash equals `content_hash`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Citation {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub content_hash: String,
}

/// One statement a page makes, with where it comes from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    /// Stable within the page (`imports.3`); prose cites facts by this id.
    pub id: String,
    pub text: String,
    pub citations: Vec<Citation>,
    /// Pages this fact points at (the level below).
    pub refs: Vec<PageId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub key: String,
    pub title: String,
    pub facts: Vec<Fact>,
    /// Facts left out because a bound was reached.
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    Ok,
    /// Something it was asked to include could not be read, or snapshot coverage is partial.
    Partial,
    /// The target changed on disk since the snapshot; the page shows the snapshot's view.
    Stale,
    /// The language is not read by any adapter.
    Unsupported,
    NotIndexed,
    NotFound,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: PageId,
    pub level: PageLevel,
    pub title: String,
    pub status: PageStatus,
    pub language: Option<String>,
    pub snapshot: String,
    /// Over the facts, their citations and links; equal digests mean equal content.
    pub digest: String,
    pub summary: String,
    pub sections: Vec<Section>,
    pub gaps: Vec<String>,
}

impl Page {
    pub fn fact_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.facts.len())
            .sum()
    }

    /// The page as Markdown, with citations as `file:start-end` and links as page ids.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# {}\n\n{}\n", self.title, self.summary);
        for section in &self.sections {
            if section.facts.is_empty() {
                continue;
            }
            out.push_str(&format!("\n## {}\n", section.title));
            for fact in &section.facts {
                out.push_str(&format!("- {}", fact.text));
                for citation in &fact.citations {
                    out.push_str(&format!(
                        " [{}:{}-{}]",
                        citation.file, citation.start_line, citation.end_line
                    ));
                }
                out.push('\n');
            }
            if section.omitted > 0 {
                out.push_str(&format!("- … {} more\n", section.omitted));
            }
        }
        if !self.gaps.is_empty() {
            out.push_str("\n## Gaps\n");
            for gap in &self.gaps {
                out.push_str(&format!("- {gap}\n"));
            }
        }
        out
    }
}

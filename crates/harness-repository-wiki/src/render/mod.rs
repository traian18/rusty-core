//! Renders pages from published snapshots: a pure function of the snapshot and, for history and
//! findings, the Git and findings sets that match it. No model, nothing stored.
mod file;
mod group;
mod history;
mod tree;

use std::sync::Arc;

use harness_findings::{FindingSet, FindingsAnalysis};
use harness_git_analysis::{GitAnalysis, GitSnapshot};
use harness_workspace::repository_intelligence::{RepositoryGraph, RepositorySnapshot, SnapshotId};

pub use tree::TreeEntry;

use crate::{
    error::WikiError,
    facts::Facts,
    model::{Fact, Page, PageId, PageLevel, PageStatus, Section},
    source::Sources,
};

/// Most facts one section lists before the rest are counted.
pub(crate) const SECTION_LIMIT: usize = 25;
/// Most facts of a short section (findings, entry points).
pub(crate) const SHORT_LIMIT: usize = 10;

/// Everything one page render reads.
pub(crate) struct Cx<'a> {
    pub graph: &'a Arc<dyn RepositoryGraph>,
    pub snapshot: &'a RepositorySnapshot,
    pub facts: Facts<'a>,
    pub sources: Sources,
    pub git: Option<(Arc<dyn GitAnalysis>, Arc<GitSnapshot>)>,
    pub findings: Option<Arc<FindingSet>>,
}

/// Renders wiki pages for one repository.
#[derive(Clone)]
pub struct PageRenderer {
    graph: Arc<dyn RepositoryGraph>,
    git: Option<Arc<dyn GitAnalysis>>,
    findings: Option<Arc<dyn FindingsAnalysis>>,
}

impl PageRenderer {
    pub fn new(graph: Arc<dyn RepositoryGraph>) -> Self {
        Self {
            graph,
            git: None,
            findings: None,
        }
    }

    pub fn with_git(mut self, git: Arc<dyn GitAnalysis>) -> Self {
        self.git = Some(git);
        self
    }

    pub fn with_findings(mut self, findings: Arc<dyn FindingsAnalysis>) -> Self {
        self.findings = Some(findings);
        self
    }

    /// The current Git snapshot, when Git analysis is attached and has run.
    pub fn git_snapshot(&self) -> Option<Arc<GitSnapshot>> {
        self.git.as_ref().and_then(|g| g.current())
    }

    pub fn graph(&self) -> &Arc<dyn RepositoryGraph> {
        &self.graph
    }

    /// The current snapshot the pages are rendered from.
    pub fn current_snapshot(&self) -> Result<Arc<RepositorySnapshot>, WikiError> {
        self.snapshot(None)
    }

    fn snapshot(&self, id: Option<&SnapshotId>) -> Result<Arc<RepositorySnapshot>, WikiError> {
        match id {
            Some(id) => Ok(self.graph.get(id)?),
            None => self.graph.current().ok_or(WikiError::NoSnapshot),
        }
    }

    /// Runs `render` with the snapshot, the Git set and the findings set that belong to it.
    fn with_context<T>(
        &self,
        snapshot: Option<&SnapshotId>,
        render: impl FnOnce(&mut Cx<'_>) -> T,
    ) -> Result<T, WikiError> {
        let snapshot = self.snapshot(snapshot)?;
        let mut cx = Cx {
            graph: &self.graph,
            snapshot: &snapshot,
            facts: Facts::new(&snapshot),
            sources: Sources::new(self.graph.clone()),
            git: self
                .git
                .as_ref()
                .and_then(|git| Some((git.clone(), git.current()?))),
            findings: self
                .findings
                .as_ref()
                .and_then(|findings| findings.current())
                .filter(|set| set.repository_snapshot == snapshot.id),
        };
        Ok(render(&mut cx))
    }

    /// The page with this id, from `snapshot` (the current one when `None`).
    pub fn page(&self, id: &PageId, snapshot: Option<&SnapshotId>) -> Result<Page, WikiError> {
        let (level, target) = id.parse()?;
        let target = target.to_owned();
        self.with_context(snapshot, |cx| match level {
            PageLevel::File => file::page(cx, id, &target),
            PageLevel::Folder | PageLevel::Package | PageLevel::Repository => {
                group::page(cx, id, level, &target)
            }
        })
    }

    /// The pages one level below `id` (the tree the Docs tab shows).
    pub fn children(
        &self,
        id: &PageId,
        snapshot: Option<&SnapshotId>,
    ) -> Result<Vec<TreeEntry>, WikiError> {
        let (level, target) = id.parse()?;
        let target = target.to_owned();
        self.with_context(snapshot, |cx| tree::children(cx, level, &target))
    }
}

/// Numbered facts of one section, bounded.
pub(crate) struct SectionBuilder {
    key: &'static str,
    title: &'static str,
    facts: Vec<Fact>,
    omitted: usize,
    limit: usize,
}

impl SectionBuilder {
    pub fn new(key: &'static str, title: &'static str, limit: usize) -> Self {
        Self {
            key,
            title,
            facts: Vec::new(),
            omitted: 0,
            limit,
        }
    }

    pub fn push(
        &mut self,
        text: String,
        citations: Vec<crate::model::Citation>,
        refs: Vec<PageId>,
    ) {
        if self.facts.len() >= self.limit {
            self.omitted += 1;
            return;
        }
        let id = format!("{}.{}", self.key, self.facts.len() + 1);
        self.facts.push(Fact {
            id,
            text,
            citations,
            refs,
        });
    }

    pub fn note_omitted(&mut self, count: usize) {
        self.omitted += count;
    }

    pub fn finish(self) -> Section {
        Section {
            key: self.key.to_owned(),
            title: self.title.to_owned(),
            facts: self.facts,
            omitted: self.omitted,
        }
    }
}

/// A page about something that cannot be described, with the reason as its status.
pub(crate) fn absent(
    id: &PageId,
    level: PageLevel,
    snapshot: &RepositorySnapshot,
    status: PageStatus,
    language: Option<String>,
    summary: String,
    gaps: Vec<String>,
) -> Page {
    Page {
        id: id.clone(),
        level,
        title: id.0.clone(),
        status,
        language,
        snapshot: snapshot.id.0.clone(),
        digest: crate::digest::sections_digest(&[]),
        summary,
        sections: Vec::new(),
        gaps,
    }
}

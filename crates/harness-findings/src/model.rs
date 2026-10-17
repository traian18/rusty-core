//! What a finding is, separate from the graph entities and edges it is derived from.
use serde::{Deserialize, Serialize};

use harness_git_analysis::{CommitId, GitSnapshotId};
use harness_workspace::{
    dependency_groups::GroupId,
    import_graph::ImportFactId,
    repository_intelligence::{PackageId, RootRelativePath, SnapshotId},
};

/// Stable identity of the form `rule:key`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FindingId(pub String);

/// Derived from facts, or a labelled heuristic. Measurable findings rank first.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Measurable,
    Candidate,
}

/// Most serious first, so ascending order is priority order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Medium,
    Low,
    Info,
}

/// How sure the rule is that the observation is right. Most confident first.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleRef {
    pub id: String,
    pub version: u32,
}

/// A byte span in a file, with its line and column when the file still matches the snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub file: RootRelativePath,
    pub start: usize,
    pub end: usize,
    /// 1-based; absent when the file changed since the snapshot or could not be read.
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The file no longer matches the snapshot's content hash, so only the byte span is trusted.
    pub stale: bool,
}

/// Historical facts a finding relied on.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitEvidence {
    pub support: Option<u32>,
    pub confidence_per_mille: Option<u32>,
    pub commits: Option<u32>,
    pub last_change: Option<i64>,
    pub example_commits: Vec<CommitId>,
}

/// What a finding rests on.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub files: Vec<RootRelativePath>,
    /// Import sites; resolve them through the repository snapshot.
    pub import_sites: Vec<ImportFactId>,
    pub groups: Vec<GroupId>,
    pub manifests: Vec<RootRelativePath>,
    pub git: Option<GitEvidence>,
    /// True when more evidence existed than the configured bound keeps.
    pub truncated: bool,
}

/// A rule's output before the analyzer adds identity, snapshots, and resolved locations.
#[derive(Clone, Debug)]
pub struct Draft {
    /// Unique within the rule.
    pub key: String,
    pub severity: Severity,
    pub confidence: Confidence,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub subjects: Vec<RootRelativePath>,
    pub packages: Vec<PackageId>,
    pub impact: usize,
    pub evidence: Evidence,
    pub spans: Vec<SourceSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub file: RootRelativePath,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: FindingId,
    pub rule: RuleRef,
    pub kind: FindingKind,
    pub severity: Severity,
    pub confidence: Confidence,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub subjects: Vec<RootRelativePath>,
    pub packages: Vec<PackageId>,
    /// Files affected, for ranking.
    pub impact: usize,
    pub evidence: Evidence,
    pub locations: Vec<Location>,
    /// Standing caveat for candidates.
    pub interpretation: Option<String>,
    pub repository_snapshot: SnapshotId,
    pub git_snapshot: Option<GitSnapshotId>,
}

impl Finding {
    /// The documented order: kind, severity, confidence, then more affected files first, then id.
    pub fn priority_key(
        &self,
    ) -> (
        FindingKind,
        Severity,
        Confidence,
        std::cmp::Reverse<usize>,
        &FindingId,
    ) {
        (
            self.kind,
            self.severity,
            self.confidence,
            std::cmp::Reverse(self.impact),
            &self.id,
        )
    }
}

/// Standing caveat attached to every candidate.
pub const CANDIDATE_INTERPRETATION: &str = "A candidate from heuristics over static imports and history; it is not evidence that the code is unused or risky.";

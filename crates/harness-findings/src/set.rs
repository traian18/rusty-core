//! The immutable result of one analysis and the queries over it.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harness_git_analysis::GitSnapshotId;
use harness_workspace::repository_intelligence::{PackageId, RootRelativePath, SnapshotId};

use crate::{
    error::FindingsQueryError,
    model::{Confidence, Finding, FindingId, FindingKind, RuleRef, Severity},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FindingSetId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkippedRule {
    pub rule: RuleRef,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SuppressedFinding {
    pub finding: Finding,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingsSummary {
    pub total: usize,
    pub suppressed: usize,
    pub by_rule: BTreeMap<String, usize>,
    pub by_kind: BTreeMap<FindingKind, usize>,
    pub by_severity: BTreeMap<Severity, usize>,
    /// Rules that could not run because an input they need was missing.
    pub skipped_rules: Vec<SkippedRule>,
    /// Rules that found more than `max_findings_per_rule`; only the highest-ranked were kept.
    pub truncated_rules: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingSet {
    pub id: FindingSetId,
    pub repository_snapshot: SnapshotId,
    pub git_snapshot: Option<GitSnapshotId>,
    pub config_digest: String,
    pub rules: Vec<RuleRef>,
    /// Ranked: kind, severity, confidence, impact (most files first), then id.
    pub findings: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
    pub summary: FindingsSummary,
    pub max_page_limit: usize,
}

impl FindingSet {
    /// Deterministic over every input that can change the content.
    pub fn compute_id(
        config_digest: &str,
        repository: &SnapshotId,
        git: &Option<GitSnapshotId>,
        rules: &[RuleRef],
        findings: &[Finding],
        suppressed: &[SuppressedFinding],
        summary: &FindingsSummary,
    ) -> FindingSetId {
        let bytes = serde_json::to_vec(&(
            "findings-v1",
            config_digest,
            repository,
            git,
            rules,
            findings,
            suppressed,
            summary,
        ))
        .unwrap_or_default();
        FindingSetId(format!("{:x}", Sha256::digest(&bytes)))
    }

    pub fn find(&self, id: &FindingId) -> Result<FindingEntry, FindingsQueryError> {
        if let Some(finding) = self.findings.iter().find(|f| f.id == *id) {
            return Ok(FindingEntry {
                finding: finding.clone(),
                suppressed_reason: None,
            });
        }
        self.suppressed
            .iter()
            .find(|s| s.finding.id == *id)
            .map(|s| FindingEntry {
                finding: s.finding.clone(),
                suppressed_reason: Some(s.reason.clone()),
            })
            .ok_or_else(|| FindingsQueryError::FindingNotFound(id.0.clone()))
    }

    pub fn list(&self, query: &FindingsListQuery) -> Result<FindingsPage, FindingsQueryError> {
        if query.limit == 0 || query.limit > self.max_page_limit {
            return Err(FindingsQueryError::InvalidLimit {
                max: self.max_page_limit,
            });
        }
        let shape = fingerprint(query);
        let offset = match &query.cursor {
            None => 0,
            Some(cursor) if cursor.set != self.id => return Err(FindingsQueryError::ForeignCursor),
            Some(cursor) if cursor.fingerprint != shape => {
                return Err(FindingsQueryError::InvalidCursor)
            }
            Some(cursor) => cursor.offset,
        };
        let ranked = self.findings.iter().map(|f| (f, None));
        let suppressed = self
            .suppressed
            .iter()
            .filter(|_| query.include_suppressed)
            .map(|s| (&s.finding, Some(&s.reason)));
        let matching = ranked
            .chain(suppressed)
            .filter(|(finding, _)| query.accepts(finding))
            .skip(offset)
            .take(query.limit + 1)
            .map(|(finding, reason)| FindingEntry {
                finding: finding.clone(),
                suppressed_reason: reason.cloned(),
            })
            .collect::<Vec<_>>();
        let more = matching.len() > query.limit;
        let entries = matching.into_iter().take(query.limit).collect();
        Ok(FindingsPage {
            set: self.id.clone(),
            entries,
            next_cursor: more.then(|| FindingsCursor {
                set: self.id.clone(),
                fingerprint: shape,
                offset: offset + query.limit,
            }),
        })
    }
}

/// Position in a paged result, bound to one finding set and one query shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingsCursor {
    pub set: FindingSetId,
    pub fingerprint: String,
    pub offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingsListQuery {
    pub set: FindingSetId,
    pub rules: Vec<String>,
    pub kind: Option<FindingKind>,
    /// Keep findings at least this severe.
    pub min_severity: Option<Severity>,
    /// Keep findings at least this confident.
    pub min_confidence: Option<Confidence>,
    pub package: Option<PackageId>,
    /// Keep findings whose subjects include this file.
    pub file: Option<RootRelativePath>,
    pub include_suppressed: bool,
    pub cursor: Option<FindingsCursor>,
    pub limit: usize,
}

impl FindingsListQuery {
    fn accepts(&self, finding: &Finding) -> bool {
        (self.rules.is_empty() || self.rules.contains(&finding.rule.id))
            && self.kind.is_none_or(|kind| finding.kind == kind)
            && self.min_severity.is_none_or(|min| finding.severity <= min)
            && self
                .min_confidence
                .is_none_or(|min| finding.confidence <= min)
            && self
                .package
                .as_ref()
                .is_none_or(|p| finding.packages.contains(p))
            && self
                .file
                .as_ref()
                .is_none_or(|f| finding.subjects.contains(f))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingEntry {
    pub finding: Finding,
    pub suppressed_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingsPage {
    pub set: FindingSetId,
    pub entries: Vec<FindingEntry>,
    pub next_cursor: Option<FindingsCursor>,
}

/// Query shape without the cursor, so every page of one query shares a fingerprint.
fn fingerprint(query: &FindingsListQuery) -> String {
    let shape = (
        &query.set,
        &query.rules,
        query.kind,
        query.min_severity,
        query.min_confidence,
        &query.package,
        &query.file,
        query.include_suppressed,
    );
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&shape).unwrap_or_default())
    )
}

//! Runs the rules, applies bounds and suppressions, resolves locations, and ranks the result.
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use tokio_util::sync::CancellationToken;

use harness_git_analysis::GitSnapshot;
use harness_workspace::repository_intelligence::RepositorySnapshot;

use crate::{
    config::FindingsConfig,
    error::FindingsError,
    model::{Draft, Finding, FindingId, FindingKind, RuleRef, CANDIDATE_INTERPRETATION},
    rules::{
        BoundaryViolationRule, ChangeHotspotRule, FindingRule, FindingsInput, ImplicitCouplingRule,
        ImportCycleRule, TypeOnlyImportCycleRule, UnattachedFileRule, UndeclaredDependencyRule,
        UndeclaredWorkspaceDependencyRule, UnreferencedFileRule, UnusedDeclaredDependencyRule,
    },
    set::{FindingSet, FindingsSummary, SkippedRule, SuppressedFinding},
    source::{Locator, SourceTextReader},
    view::RepoView,
};

pub struct AnalysisInput<'a> {
    pub repository: &'a RepositorySnapshot,
    pub git: Option<&'a GitSnapshot>,
    pub reader: &'a dyn SourceTextReader,
}

/// Turns snapshots into a [`FindingSet`]. Pure: the same inputs and rule versions give the same set.
pub trait FindingsAnalyzer: Send + Sync {
    fn analyze(
        &self,
        input: &AnalysisInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<FindingSet, FindingsError>;
    fn config(&self) -> &FindingsConfig;
    /// One-line description of what a rule observes.
    fn describe_rule(&self, rule_id: &str) -> Option<&'static str>;
}

/// Runs registered rules in order.
pub struct DefaultFindingsAnalyzer {
    rules: Vec<Arc<dyn FindingRule>>,
    config: FindingsConfig,
}

impl DefaultFindingsAnalyzer {
    /// The measurable, policy, and candidate rules of the first slice.
    pub fn standard(config: FindingsConfig) -> Result<Self, FindingsError> {
        Self::with_rules(
            vec![
                Arc::new(ImportCycleRule),
                Arc::new(TypeOnlyImportCycleRule),
                Arc::new(UndeclaredDependencyRule),
                Arc::new(UndeclaredWorkspaceDependencyRule),
                Arc::new(UnattachedFileRule),
                Arc::new(BoundaryViolationRule),
                Arc::new(UnreferencedFileRule),
                Arc::new(ImplicitCouplingRule),
                Arc::new(ChangeHotspotRule),
                Arc::new(UnusedDeclaredDependencyRule),
            ],
            config,
        )
    }

    pub fn with_rules(
        rules: Vec<Arc<dyn FindingRule>>,
        config: FindingsConfig,
    ) -> Result<Self, FindingsError> {
        config.validate()?;
        Ok(Self { rules, config })
    }

    /// The skip reason when a rule's inputs are missing.
    fn unmet_requirement(
        rule: &dyn FindingRule,
        input: &AnalysisInput<'_>,
    ) -> Option<&'static str> {
        let needs = rule.requires();
        if needs.groups && !input.repository.groups.configured {
            Some("dependency groups are not configured for this repository graph")
        } else if needs.git && input.git.is_none() {
            Some("no Git snapshot was supplied")
        } else {
            None
        }
    }
}

impl FindingsAnalyzer for DefaultFindingsAnalyzer {
    fn analyze(
        &self,
        input: &AnalysisInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<FindingSet, FindingsError> {
        let view = RepoView::new(input.repository);
        let rule_input = FindingsInput {
            repository: input.repository,
            git: input.git,
            config: &self.config,
            view: &view,
        };
        let hashes = input
            .repository
            .imports
            .files
            .iter()
            .map(|file| (&file.path, file.content_hash.as_str()))
            .collect::<HashMap<_, _>>();
        let mut locator = Locator::new(input.reader, self.config.max_source_bytes, &hashes);
        let mut findings = Vec::new();
        let mut summary = FindingsSummary::default();
        for rule in &self.rules {
            if cancel.is_cancelled() {
                return Err(FindingsError::Cancelled);
            }
            let reference = RuleRef {
                id: rule.id().to_owned(),
                version: rule.version(),
            };
            if let Some(reason) = Self::unmet_requirement(rule.as_ref(), input) {
                summary.skipped_rules.push(SkippedRule {
                    rule: reference,
                    reason: reason.to_owned(),
                });
                continue;
            }
            let mut drafts = rule.evaluate(&rule_input);
            drafts.sort_by(|a, b| {
                (
                    a.severity,
                    a.confidence,
                    std::cmp::Reverse(a.impact),
                    &a.key,
                )
                    .cmp(&(
                        b.severity,
                        b.confidence,
                        std::cmp::Reverse(b.impact),
                        &b.key,
                    ))
            });
            if drafts.len() > self.config.max_findings_per_rule {
                drafts.truncate(self.config.max_findings_per_rule);
                summary.truncated_rules.push(reference.id.clone());
            }
            findings.extend(
                drafts.into_iter().map(|draft| {
                    self.finding(rule.as_ref(), &reference, draft, input, &mut locator)
                }),
            );
        }
        Ok(self.assemble(input, findings, summary))
    }

    fn config(&self) -> &FindingsConfig {
        &self.config
    }

    fn describe_rule(&self, rule_id: &str) -> Option<&'static str> {
        self.rules
            .iter()
            .find(|rule| rule.id() == rule_id)
            .map(|rule| rule.description())
    }
}

impl DefaultFindingsAnalyzer {
    fn finding(
        &self,
        rule: &dyn FindingRule,
        reference: &RuleRef,
        draft: Draft,
        input: &AnalysisInput<'_>,
        locator: &mut Locator<'_>,
    ) -> Finding {
        let Draft {
            key,
            severity,
            confidence,
            title,
            summary,
            rationale,
            subjects,
            packages,
            impact,
            evidence,
            spans,
        } = draft;
        Finding {
            id: FindingId(format!("{}:{key}", reference.id)),
            rule: reference.clone(),
            kind: rule.kind(),
            severity,
            confidence,
            title,
            summary,
            rationale,
            subjects,
            packages,
            impact,
            evidence,
            locations: spans.iter().map(|span| locator.locate(span)).collect(),
            interpretation: (rule.kind() == FindingKind::Candidate)
                .then(|| CANDIDATE_INTERPRETATION.to_owned()),
            repository_snapshot: input.repository.id.clone(),
            git_snapshot: input.git.map(|git| git.id.clone()),
        }
    }

    /// Splits off suppressed findings, ranks the rest, and fills in the summary and identity.
    fn assemble(
        &self,
        input: &AnalysisInput<'_>,
        findings: Vec<Finding>,
        mut summary: FindingsSummary,
    ) -> FindingSet {
        let (mut ranked, mut suppressed) = (Vec::new(), Vec::new());
        for finding in findings {
            match self
                .config
                .suppressions
                .iter()
                .find(|s| s.matches(&finding.rule.id, key_of(&finding)))
            {
                Some(suppression) => suppressed.push(SuppressedFinding {
                    finding,
                    reason: suppression.reason.clone(),
                }),
                None => ranked.push(finding),
            }
        }
        ranked.sort_by(|a, b| a.priority_key().cmp(&b.priority_key()));
        suppressed.sort_by(|a, b| a.finding.priority_key().cmp(&b.finding.priority_key()));
        summary.total = ranked.len();
        summary.suppressed = suppressed.len();
        let mut by_rule: BTreeMap<String, usize> = BTreeMap::new();
        for finding in &ranked {
            *by_rule.entry(finding.rule.id.clone()).or_default() += 1;
            *summary.by_kind.entry(finding.kind).or_default() += 1;
            *summary.by_severity.entry(finding.severity).or_default() += 1;
        }
        summary.by_rule = by_rule;
        let rules = self
            .rules
            .iter()
            .map(|rule| RuleRef {
                id: rule.id().to_owned(),
                version: rule.version(),
            })
            .collect::<Vec<_>>();
        let git = input.git.map(|git| git.id.clone());
        let id = FindingSet::compute_id(
            &self.config.digest(),
            &input.repository.id,
            &git,
            &rules,
            &ranked,
            &suppressed,
            &summary,
        );
        FindingSet {
            id,
            repository_snapshot: input.repository.id.clone(),
            git_snapshot: git,
            config_digest: self.config.digest(),
            rules,
            findings: ranked,
            suppressed,
            summary,
            max_page_limit: self.config.max_page_limit,
        }
    }
}

/// The rule-local key of a finding id (`rule:key`).
fn key_of(finding: &Finding) -> &str {
    finding
        .id
        .0
        .strip_prefix(&format!("{}:", finding.rule.id))
        .unwrap_or(&finding.id.0)
}

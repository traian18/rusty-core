//! Thresholds, the boundary policy, and suppressions. All of it is part of the finding-set identity.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harness_workspace::repository_intelligence::RootRelativePath;

use crate::model::Severity;

/// Chooses files by their package name or path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    Package(String),
    PathPrefix(RootRelativePath),
}

/// An import from `from` to `to` that the repository forbids.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoundaryRule {
    pub id: String,
    pub from: Selector,
    pub to: Selector,
    #[serde(default = "default_boundary_severity")]
    pub severity: Severity,
    pub reason: String,
}

fn default_boundary_severity() -> Severity {
    Severity::High
}

/// Empty by default: with no policy, `boundary-violation` reports nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoundaryPolicy {
    pub rules: Vec<BoundaryRule>,
}

/// Hides a finding from the ranked list without losing it: the set still counts and lists it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Suppression {
    pub rule: String,
    /// Exact finding key, or a prefix ending in `*`.
    pub key: String,
    /// Required: why this is acceptable.
    pub reason: String,
}

impl Suppression {
    pub fn matches(&self, rule: &str, key: &str) -> bool {
        self.rule == rule
            && match self.key.strip_suffix('*') {
                Some(prefix) => key.starts_with(prefix),
                None => self.key == key,
            }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingsConfig {
    pub max_findings_per_rule: usize,
    pub max_evidence_per_finding: usize,
    pub max_page_limit: usize,
    /// Source files larger than this get byte spans without line numbers.
    pub max_source_bytes: u64,
    pub implicit_min_support: u32,
    pub implicit_min_confidence_per_mille: u32,
    pub implicit_max_hops: usize,
    pub hotspot_churn_percentile: u32,
    pub hotspot_min_commits: u32,
    pub hotspot_min_dependents: usize,
    pub hotspot_dependent_hops: usize,
    pub policy: BoundaryPolicy,
    pub suppressions: Vec<Suppression>,
}

impl Default for FindingsConfig {
    fn default() -> Self {
        Self {
            max_findings_per_rule: 200,
            max_evidence_per_finding: 50,
            max_page_limit: 200,
            max_source_bytes: 2 * 1024 * 1024,
            implicit_min_support: 5,
            implicit_min_confidence_per_mille: 600,
            implicit_max_hops: 4,
            hotspot_churn_percentile: 95,
            hotspot_min_commits: 10,
            hotspot_min_dependents: 10,
            hotspot_dependent_hops: 2,
            policy: BoundaryPolicy::default(),
            suppressions: Vec::new(),
        }
    }
}

impl FindingsConfig {
    pub fn validate(&self) -> Result<(), crate::error::FindingsError> {
        let invalid = |message: &str| {
            Err(crate::error::FindingsError::InvalidConfig(
                message.to_owned(),
            ))
        };
        if self.max_findings_per_rule == 0
            || self.max_evidence_per_finding == 0
            || self.max_page_limit == 0
        {
            return invalid("findings, evidence and page limits must be positive");
        }
        if self.hotspot_churn_percentile == 0 || self.hotspot_churn_percentile > 100 {
            return invalid("hotspot_churn_percentile must be between 1 and 100");
        }
        if self.implicit_min_confidence_per_mille > 1000 {
            return invalid("implicit_min_confidence_per_mille must be at most 1000");
        }
        if let Some(rule) = self
            .suppressions
            .iter()
            .find(|s| s.reason.trim().is_empty())
        {
            return invalid(&format!(
                "suppression of {}:{} needs a reason",
                rule.rule, rule.key
            ));
        }
        if let Some(rule) = self.policy.rules.iter().find(|r| r.id.trim().is_empty()) {
            return invalid(&format!("boundary rule for {:?} needs an id", rule.from));
        }
        Ok(())
    }

    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }
}

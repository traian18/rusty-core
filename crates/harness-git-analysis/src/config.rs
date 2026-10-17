//! Window, exclusion, and threshold settings. All of them are part of the snapshot identity.
use serde::{Deserialize, Serialize};

use crate::error::GitAnalysisError;

/// Paths that are generated or vendored; a change to them says little about the code.
const DEFAULT_EXCLUSIONS: [&str; 14] = [
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "dist/",
    "build/",
    "target/",
    "node_modules/",
    "vendor/",
    "__snapshots__/",
    "*.min.*",
    "*.snap",
    "*.map",
    "*.lock",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitAnalysisConfig {
    /// Commits taken from `HEAD`, newest first, merges included in the count.
    pub max_commits: usize,
    /// Commits older than this many days before the `HEAD` commit are left out.
    pub max_age_days: u32,
    /// Hard cap on commits examined, whatever the other limits say.
    pub max_walk: usize,
    /// Commits touching more files than this still count for churn but not co-change.
    pub max_files_per_commit: usize,
    /// Commits changing more files than this get no per-file line statistics.
    pub max_line_stat_files: usize,
    pub min_support: u32,
    /// Minimum confidence in thousandths (300 = 0.3).
    pub min_confidence_per_mille: u32,
    pub max_partners_per_file: usize,
    /// Example commits recorded as evidence per co-change.
    pub max_examples: usize,
    pub max_page_limit: usize,
    /// Glob-like patterns: `dir/` matches a directory anywhere, `*.ext` matches file names.
    pub exclusions: Vec<String>,
}

impl Default for GitAnalysisConfig {
    fn default() -> Self {
        Self {
            max_commits: 500,
            max_age_days: 182,
            max_walk: 10_000,
            max_files_per_commit: 50,
            max_line_stat_files: 2_000,
            min_support: 3,
            min_confidence_per_mille: 300,
            max_partners_per_file: 20,
            max_examples: 3,
            max_page_limit: 500,
            exclusions: DEFAULT_EXCLUSIONS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

impl GitAnalysisConfig {
    pub fn validate(&self) -> Result<(), GitAnalysisError> {
        let invalid = |message: &str| Err(GitAnalysisError::InvalidConfig(message.to_owned()));
        if self.max_commits == 0 || self.max_commits > self.max_walk {
            return invalid("max_commits must be between 1 and max_walk");
        }
        if self.max_age_days == 0 {
            return invalid("max_age_days must be positive");
        }
        if self.max_files_per_commit < 2 {
            return invalid("max_files_per_commit must be at least 2");
        }
        if self.min_support == 0 || self.min_confidence_per_mille > 1000 {
            return invalid("min_support must be positive and confidence at most 1000");
        }
        if self.max_partners_per_file == 0 || self.max_page_limit == 0 {
            return invalid("partner and page limits must be positive");
        }
        Ok(())
    }

    /// Stable digest of every setting, for snapshot identity.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }
}

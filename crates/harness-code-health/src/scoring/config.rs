//! The numbers scoring runs on. Defaults are a stated prior taken from the published reference
//! (`docs/design/code-health-reference-analysis.md`): structure above history, history only
//! amplifying code risk, maintainability smells at reduced defect weight. They are replaced only
//! by calibration on our own corpus, never tuned by hand to make a repository look better.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Category, Severity};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeverityDeductions {
    pub low: f64,
    pub medium: f64,
    pub high: f64,
    pub critical: f64,
}

impl Default for SeverityDeductions {
    fn default() -> Self {
        Self {
            low: 0.3,
            medium: 0.7,
            high: 1.2,
            critical: 2.0,
        }
    }
}

impl SeverityDeductions {
    pub fn of(&self, severity: Severity) -> f64 {
        match severity {
            Severity::Low => self.low,
            Severity::Medium => self.medium,
            Severity::High => self.high,
            Severity::Critical => self.critical,
        }
    }
}

/// One dimension: which detectors count, how much each weighs, which category it is capped in
/// and the caps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DimensionConfig {
    /// Detectors that deduct in this dimension; others are ignored by it.
    pub detectors: Vec<String>,
    /// Weight per detector; a detector not listed weighs 1.0.
    pub weights: BTreeMap<String, f64>,
    /// Category per detector when it differs from the finding's own.
    pub categories: BTreeMap<String, Category>,
    /// Most a category may deduct; a category not listed is capped at 1.0.
    pub caps: BTreeMap<Category, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoringConfig {
    pub severity: SeverityDeductions,
    /// The headline: likelihood of defects.
    pub defect: DimensionConfig,
    /// How hard the code is to change, from code shape only.
    pub maintainability: DimensionConfig,
    /// History may deduct at most `history_base + history_per_structure × structure`, where
    /// structure is what every other defect category deducts (after its cap), up to the history
    /// category's own cap. A file with no code findings keeps at least `10 − history_base`.
    pub history_base: f64,
    pub history_per_structure: f64,
}

const CODE_SHAPE: [&str; 9] = [
    "nested_complexity",
    "complex_conditional",
    "god_class",
    "brain_method",
    "low_cohesion",
    "bumpy_road",
    "complex_method",
    "large_method",
    "primitive_obsession",
];

/// Code quality beyond shape: errors that are swallowed, copied code.
const QUALITY: [&str; 3] = ["duplicated_code", "swallowed_error", "panic_prone"];

/// Tests around the code; they count toward defect risk, not toward how hard the code is to change.
const TESTS: [&str; 3] = ["untested_code", "test_without_assertions", "skipped_tests"];

const HISTORY: [&str; 7] = [
    "change_entropy",
    "ownership_risk",
    "churn_risk",
    "prior_defect",
    "developer_congestion",
    "knowledge_loss",
    "co_change_scatter",
];

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

impl Default for ScoringConfig {
    fn default() -> Self {
        let defect_weights = [
            ("co_change_scatter", 1.8),
            ("change_entropy", 1.51),
            ("ownership_risk", 1.38),
            ("nested_complexity", 1.34),
            ("complex_conditional", 1.33),
            ("large_method", 1.25),
            ("complex_method", 1.21),
            ("churn_risk", 1.2),
            ("god_class", 1.13),
            ("prior_defect", 1.0),
            ("developer_congestion", 0.5),
            ("low_cohesion", 0.5),
            ("brain_method", 0.5),
            ("bumpy_road", 0.5),
            ("primitive_obsession", 0.5),
            ("knowledge_loss", 0.4),
            ("duplicated_code", 1.0),
            ("swallowed_error", 1.0),
            ("panic_prone", 0.5),
            ("untested_code", 1.0),
            ("test_without_assertions", 0.5),
            ("skipped_tests", 0.3),
        ];
        let defect_caps = [
            (Category::Organizational, 3.5),
            (Category::StructuralComplexity, 2.5),
            (Category::TestCoverage, 2.0),
            (Category::SizeAndComplexity, 1.5),
            (Category::Duplication, 1.0),
            (Category::TestQuality, 0.5),
            (Category::ErrorHandling, 0.5),
            (Category::Infrastructure, 3.0),
            (Category::Security, 4.0),
        ];
        // In maintainability, length and tangled conditions are structure, not size.
        let maintainability_categories = [
            ("large_method", Category::StructuralComplexity),
            ("complex_conditional", Category::StructuralComplexity),
        ];
        let maintainability_caps = [
            (Category::StructuralComplexity, 4.0),
            (Category::SizeAndComplexity, 2.0),
            (Category::Duplication, 2.0),
            (Category::ErrorHandling, 2.0),
            (Category::Infrastructure, 4.0),
        ];
        Self {
            severity: SeverityDeductions::default(),
            defect: DimensionConfig {
                detectors: [
                    owned(&CODE_SHAPE),
                    owned(&QUALITY),
                    owned(&TESTS),
                    owned(&HISTORY),
                ]
                .concat(),
                weights: defect_weights
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), *v))
                    .collect(),
                categories: BTreeMap::new(),
                caps: defect_caps.into_iter().collect(),
            },
            maintainability: DimensionConfig {
                detectors: [owned(&CODE_SHAPE), owned(&QUALITY)].concat(),
                weights: BTreeMap::new(),
                categories: maintainability_categories
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), *v))
                    .collect(),
                caps: maintainability_caps.into_iter().collect(),
            },
            history_base: 1.0,
            history_per_structure: 1.0,
        }
    }
}

impl ScoringConfig {
    /// Stable digest of every number scoring uses.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }
}

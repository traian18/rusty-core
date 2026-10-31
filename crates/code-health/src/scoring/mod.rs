//! From findings to scores: a file starts at 10, each finding deducts its severity's points times
//! its detector's weight, each category's deductions are capped, and the score is clamped to
//! `[1, 10]`. History may deduct a lot only where the code itself is also bad.
//!
//! Two dimensions: **defect** (the headline: every detector, calibrated-style weights, history
//! conditioned on structure) and **maintainability** (code shape only, every smell at full weight,
//! its own caps). Adding maintainability never changes the defect score.
mod config;
mod kernel;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use config::{DimensionConfig, ScoringConfig, SeverityDeductions};
pub use kernel::{CategoryDeduction, SCORE_FLOOR, SCORE_MAX};

use crate::model::{Category, Finding};

/// The category history findings are scored in.
pub const HISTORY_CATEGORY: Category = Category::Organizational;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DimensionScore {
    /// In `[1, 10]`, rounded to hundredths.
    pub score: f64,
    /// `10 − total deduction`, which may fall below 1: the floor hides progress on very bad files.
    pub unclamped: f64,
    pub categories: BTreeMap<Category, CategoryDeduction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileScore {
    pub defect: DimensionScore,
    pub maintainability: DimensionScore,
    /// What the code itself deducts from the defect score, and what its history deducts. They sum
    /// to the defect deduction, so a surface can say which half holds the score down.
    pub structure_deduction: f64,
    pub history_deduction: f64,
    /// Each finding's share of the defect deduction, by finding id, rounded to thousandths.
    pub impacts: BTreeMap<String, f64>,
}

fn round(value: f64, places: i32) -> f64 {
    let factor = 10f64.powi(places);
    (value * factor).round() / factor
}

fn dimension(kernel: &kernel::Kernel) -> DimensionScore {
    DimensionScore {
        score: round(kernel.score, 2),
        unclamped: round(SCORE_MAX - kernel.total, 2),
        categories: kernel.categories.clone(),
    }
}

/// Scores one file from its findings. Findings of detectors a dimension does not list are
/// ignored by that dimension.
pub fn score_file(findings: &[Finding], config: &ScoringConfig) -> FileScore {
    let history_cap =
        |structure: f64| config.history_base + config.history_per_structure * structure.max(0.0);
    let defect = kernel::run(
        findings,
        &config.severity,
        &config.defect,
        Some((HISTORY_CATEGORY, &history_cap)),
    );
    let maintainability = kernel::run(findings, &config.severity, &config.maintainability, None);
    let history: f64 = defect
        .categories
        .get(&HISTORY_CATEGORY)
        .map_or(0.0, |c| c.applied);
    FileScore {
        structure_deduction: round(defect.total - history, 3),
        history_deduction: round(history, 3),
        impacts: findings
            .iter()
            .zip(&defect.impacts)
            .map(|(f, impact)| (f.id.clone(), round(*impact, 3)))
            .collect(),
        defect: dimension(&defect),
        maintainability: dimension(&maintainability),
    }
}

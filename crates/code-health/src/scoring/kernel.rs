//! The one scoring algorithm every dimension runs with its own tables.
use std::collections::BTreeMap;

use super::config::{DimensionConfig, SeverityDeductions};
use crate::model::{Category, Finding};

pub const SCORE_MAX: f64 = 10.0;
pub const SCORE_FLOOR: f64 = 1.0;

/// A category's deduction before and after its cap.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CategoryDeduction {
    pub raw: f64,
    pub cap: f64,
    pub applied: f64,
}

pub struct Kernel {
    pub score: f64,
    pub total: f64,
    /// Each finding's share of the deduction after capping, parallel to the findings (0.0 for
    /// findings this dimension ignores).
    pub impacts: Vec<f64>,
    pub categories: BTreeMap<Category, CategoryDeduction>,
}

/// Weights each finding, sums per category, caps each category (scaling its findings down
/// proportionally so every finding's share stays attributable), and clamps `10 − total` into
/// `[1, 10]`. `conditioned` replaces one category's cap with a function of what every other
/// category deducts after its cap.
pub fn run(
    findings: &[Finding],
    severity: &SeverityDeductions,
    dimension: &DimensionConfig,
    conditioned: Option<(Category, &dyn Fn(f64) -> f64)>,
) -> Kernel {
    let mut by_category: BTreeMap<Category, Vec<(usize, f64)>> = BTreeMap::new();
    for (index, finding) in findings.iter().enumerate() {
        if !dimension.detectors.contains(&finding.detector) {
            continue;
        }
        let weight = dimension
            .weights
            .get(&finding.detector)
            .copied()
            .unwrap_or(1.0);
        let category = dimension
            .categories
            .get(&finding.detector)
            .copied()
            .unwrap_or(finding.category);
        by_category
            .entry(category)
            .or_default()
            .push((index, severity.of(finding.severity) * weight));
    }
    let cap_of = |category: &Category| dimension.caps.get(category).copied().unwrap_or(1.0);
    let mut caps: BTreeMap<Category, f64> = by_category.keys().map(|c| (*c, cap_of(c))).collect();
    if let Some((category, cap_fn)) = conditioned {
        let others: f64 = by_category
            .iter()
            .filter(|(c, _)| **c != category)
            .map(|(c, entries)| entries.iter().map(|(_, d)| d).sum::<f64>().min(caps[c]))
            .sum();
        if caps.contains_key(&category) {
            caps.insert(category, cap_fn(others).min(cap_of(&category)));
        }
    }
    let mut impacts = vec![0.0; findings.len()];
    let mut categories = BTreeMap::new();
    let mut total = 0.0;
    for (category, entries) in &by_category {
        let raw: f64 = entries.iter().map(|(_, d)| d).sum();
        let cap = caps[category];
        let scale = if raw > cap && raw > 0.0 {
            cap / raw
        } else {
            1.0
        };
        for (index, d) in entries {
            impacts[*index] = d * scale;
        }
        let applied = raw.min(cap);
        total += applied;
        categories.insert(*category, CategoryDeduction { raw, cap, applied });
    }
    Kernel {
        score: (SCORE_MAX - total).clamp(SCORE_FLOOR, SCORE_MAX),
        total,
        impacts,
        categories,
    }
}

//! Scoring that counts infrastructure rules next to the code detectors.
use harness_code_health::{Category, ScoringConfig};

use crate::rules::registry;

/// The default scoring configuration plus every infrastructure rule, weighted by category.
pub fn scoring_config() -> ScoringConfig {
    let mut config = ScoringConfig::default();
    for rule in registry() {
        let id = rule.id().to_owned();
        let weight = match rule.category() {
            Category::Security => 1.5,
            _ => 1.0,
        };
        config.defect.detectors.push(id.clone());
        config.defect.weights.insert(id.clone(), weight);
        config.maintainability.detectors.push(id);
    }
    config
}

//! The registry: every detector, in a fixed order, and the one call that runs them on a file.
mod brain_method;
mod bumpy_road;
mod complex_conditional;
mod complex_method;
mod god_class;
mod history;
mod large_method;
mod low_cohesion;
mod nested_complexity;
mod primitive_obsession;

pub use brain_method::BrainMethod;
pub use bumpy_road::BumpyRoad;
pub use complex_conditional::ComplexConditional;
pub use complex_method::ComplexMethod;
pub use god_class::GodClass;
pub use history::{
    ChangeEntropy, ChurnRisk, CoChangeScatter, DeveloperCongestion, KnowledgeLoss, OwnershipRisk,
    PriorDefect,
};
pub use large_method::LargeMethod;
pub use low_cohesion::LowCohesion;
pub use nested_complexity::NestedComplexity;
pub use primitive_obsession::PrimitiveObsession;

use crate::{config::DetectorConfig, context::FileContext, detector::Detector, model::Finding};

/// Every detector: structural complexity, then size and complexity, then history.
pub fn registry() -> Vec<Box<dyn Detector>> {
    vec![
        Box::new(NestedComplexity),
        Box::new(ComplexConditional),
        Box::new(GodClass),
        Box::new(BrainMethod),
        Box::new(LowCohesion),
        Box::new(BumpyRoad),
        Box::new(ComplexMethod),
        Box::new(LargeMethod),
        Box::new(PrimitiveObsession),
        Box::new(ChangeEntropy),
        Box::new(OwnershipRisk),
        Box::new(ChurnRisk),
        Box::new(PriorDefect),
        Box::new(DeveloperCongestion),
        Box::new(KnowledgeLoss),
        Box::new(CoChangeScatter),
    ]
}

/// Runs every enabled detector on one file; findings sorted by line, then detector.
pub fn detect_file(context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
    let mut findings: Vec<Finding> = registry()
        .iter()
        .filter(|d| !config.disabled.contains(d.id()))
        .flat_map(|d| d.detect(context, config))
        .collect();
    findings.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then(a.detector.cmp(&b.detector))
            .then(a.id.cmp(&b.id))
    });
    findings
}

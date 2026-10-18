use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions with several separate blocks of nested logic one after another.
pub struct BumpyRoad;

impl Detector for BumpyRoad {
    fn id(&self) -> &'static str {
        "bumpy_road"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Functions with several separate blocks of nested logic."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.bumpy_road;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.bumps >= c.threshold && f.ccn >= c.min_ccn)
            .map(|f| {
                let severity = step(f.bumps, Severity::Low, &[(c.medium, Severity::Medium), (c.high, Severity::High)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("bumps", f.bumps), ("ccn", f.ccn), ("limit", c.threshold)],
                    reason: format!("{} has {} separate blocks of nested logic; each is a step that could be its own function.", code(&f.qualified_name), f.bumps),
                })
            })
            .collect()
    }
}

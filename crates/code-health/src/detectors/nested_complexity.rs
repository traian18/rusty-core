use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions whose control blocks nest too deep to follow.
pub struct NestedComplexity;

impl Detector for NestedComplexity {
    fn id(&self) -> &'static str {
        "nested_complexity"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Functions with deeply nested control blocks."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.nested_complexity;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.max_nesting >= c.threshold)
            .map(|f| {
                let severity = step(f.max_nesting, Severity::Medium, &[(c.high, Severity::High), (c.critical, Severity::Critical)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("max_nesting", f.max_nesting), ("limit", c.threshold)],
                    reason: format!("{} nests control blocks {} deep (limit {}); pull the inner blocks into functions or return early.", code(&f.qualified_name), f.max_nesting, c.threshold),
                })
            })
            .collect()
    }
}

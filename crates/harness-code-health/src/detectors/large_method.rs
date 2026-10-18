use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions too long to hold in mind, unless they are straight-line code.
pub struct LargeMethod;

impl Detector for LargeMethod {
    fn id(&self) -> &'static str {
        "large_method"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::SizeAndComplexity
    }
    fn summary(&self) -> &'static str {
        "Long functions that also branch."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.large_method;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.nloc >= c.threshold && f.ccn >= c.min_ccn)
            .map(|f| {
                let severity = step(f.nloc, Severity::Low, &[(c.medium, Severity::Medium), (c.high, Severity::High), (c.critical, Severity::Critical)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("nloc", f.nloc), ("ccn", f.ccn), ("limit", c.threshold)],
                    reason: format!("{} is {} lines of code (limit {}); extract its steps into named functions.", code(&f.qualified_name), f.nloc, c.threshold),
                })
            })
            .collect()
    }
}

use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions with many independent paths (cyclomatic complexity).
pub struct ComplexMethod;

impl Detector for ComplexMethod {
    fn id(&self) -> &'static str {
        "complex_method"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::SizeAndComplexity
    }
    fn summary(&self) -> &'static str {
        "Functions with high cyclomatic complexity."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.complex_method;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.ccn >= c.threshold)
            .map(|f| {
                let severity = step(f.ccn, Severity::Medium, &[(c.high, Severity::High), (c.critical, Severity::Critical)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("ccn", f.ccn), ("limit", c.threshold)],
                    reason: format!("{} has cyclomatic complexity {} (limit {}): {} paths to test; split the decisions apart.", code(&f.qualified_name), f.ccn, c.threshold, f.ccn),
                })
            })
            .collect()
    }
}

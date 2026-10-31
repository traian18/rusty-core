use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions that are long, complex and in a file many others depend on: where a change is both
/// hard to make and felt far away. Silent when the file's importers are unknown.
pub struct BrainMethod;

impl Detector for BrainMethod {
    fn id(&self) -> &'static str {
        "brain_method"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Long, complex functions in files many others import."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.brain_method;
        let Some(importers) = context.importers else {
            return Vec::new();
        };
        // Relative to the repository, so a sparsely connected language still has central files.
        let floor = match context.repo_importers_p80 {
            Some(p80) => c.max_floor.min(p80.max(c.min_floor)),
            None => c.max_floor,
        };
        if importers < floor {
            return Vec::new();
        }
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.nloc >= c.min_nloc && f.ccn >= c.min_ccn)
            .map(|f| {
                let severity = if f.ccn >= c.critical_ccn && f.nloc >= c.critical_nloc {
                    Severity::Critical
                } else if f.ccn >= c.high_ccn || f.nloc >= c.high_nloc {
                    Severity::High
                } else {
                    Severity::Medium
                };
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("nloc", f.nloc), ("ccn", f.ccn), ("importers", importers), ("importers_floor", floor)],
                    reason: format!(
                        "{} is {} lines with CCN {} in a file {} others import; changes here are hard and travel far, so split it before changing it further.",
                        code(&f.qualified_name), f.nloc, f.ccn, importers
                    ),
                })
            })
            .collect()
    }
}

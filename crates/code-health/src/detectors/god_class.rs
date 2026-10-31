use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Large classes with many methods that also hide one long, complex method.
pub struct GodClass;

impl Detector for GodClass {
    fn id(&self) -> &'static str {
        "god_class"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Large classes with many methods and a brain method inside."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.god_class;
        let functions = &context.metrics.functions;
        context
            .metrics
            .classes
            .iter()
            .filter(|class| class.nloc >= c.min_nloc && class.method_count >= c.min_methods)
            .filter_map(|class| {
                let brain = class
                    .methods
                    .iter()
                    .map(|i| &functions[*i])
                    .filter(|m| m.nloc >= c.brain_nloc && m.ccn >= c.brain_ccn)
                    .max_by_key(|m| (m.ccn, m.nloc))?;
                let severity = if class.nloc >= c.critical_nloc && class.method_count >= c.critical_methods {
                    Severity::Critical
                } else if class.nloc >= c.high_nloc || class.method_count >= c.high_methods {
                    Severity::High
                } else {
                    Severity::Medium
                };
                Some(finding(self, context, Draft {
                    severity,
                    function: None,
                    class: Some(class.name.clone()),
                    lines: (class.start_line, class.end_line),
                    measured: vec![("nloc", class.nloc), ("methods", class.method_count), ("brain_nloc", brain.nloc), ("brain_ccn", brain.ccn)],
                    reason: format!(
                        "{} has {} methods in {} lines and centres on {} ({} lines, CCN {}); split it by responsibility.",
                        code(&class.name), class.method_count, class.nloc, code(&brain.qualified_name), brain.nloc, brain.ccn
                    ),
                }))
            })
            .collect()
    }
}

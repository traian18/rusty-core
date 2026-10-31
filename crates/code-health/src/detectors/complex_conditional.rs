use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Boolean expressions glued from many `&&` / `||`. A flat chain of one operator with no
/// negation reads as a list of alternatives, so it stays low however long it is.
pub struct ComplexConditional;

impl Detector for ComplexConditional {
    fn id(&self) -> &'static str {
        "complex_conditional"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Conditions combining many boolean operators."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.complex_conditional;
        let mut out = Vec::new();
        for f in &context.metrics.functions {
            for condition in f.conditions.iter().filter(|x| x.operators >= c.threshold) {
                let flat = !condition.mixed && !condition.negated;
                let severity = if flat {
                    Severity::Low
                } else {
                    step(
                        condition.operators,
                        Severity::Low,
                        &[
                            (c.medium, Severity::Medium),
                            (c.high, Severity::High),
                            (c.critical, Severity::Critical),
                        ],
                    )
                };
                let shape = if flat {
                    "a flat chain of one operator"
                } else if condition.mixed {
                    "mixing `&&` and `||`"
                } else {
                    "with negations"
                };
                out.push(finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (condition.line, condition.line),
                    measured: vec![("operators", condition.operators), ("limit", c.threshold), ("mixed", u32::from(condition.mixed)), ("negated", u32::from(condition.negated))],
                    reason: format!("A condition in {} has {} boolean operators, {shape}; name its parts as variables or a function.", code(&f.qualified_name), condition.operators),
                }));
            }
        }
        out
    }
}

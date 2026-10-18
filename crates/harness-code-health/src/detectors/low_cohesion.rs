use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Classes whose methods fall into unrelated groups (LCOM4 of 2 or more).
pub struct LowCohesion;

impl Detector for LowCohesion {
    fn id(&self) -> &'static str {
        "low_cohesion"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::StructuralComplexity
    }
    fn summary(&self) -> &'static str {
        "Classes whose methods form unrelated groups."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.low_cohesion;
        context
            .metrics
            .classes
            .iter()
            .filter(|class| class.lcom4 >= c.min_lcom4 && class.method_count >= c.min_methods)
            .map(|class| {
                let severity = if class.lcom4 >= c.critical_lcom4 && class.method_count >= c.critical_methods {
                    Severity::Critical
                } else if class.lcom4 >= c.high_lcom4 || class.method_count >= c.high_methods {
                    Severity::High
                } else {
                    Severity::Medium
                };
                finding(self, context, Draft {
                    severity,
                    function: None,
                    class: Some(class.name.clone()),
                    lines: (class.start_line, class.end_line),
                    measured: vec![("lcom4", class.lcom4), ("methods", class.method_count), ("fields", class.field_count)],
                    reason: format!(
                        "{}'s {} methods form {} groups that share no fields or calls; each group could be its own type.",
                        code(&class.name), class.method_count, class.lcom4
                    ),
                })
            })
            .collect()
    }
}

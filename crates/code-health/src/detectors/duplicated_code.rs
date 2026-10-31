use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Functions that exist in near-identical form elsewhere in the repository.
pub struct DuplicatedCode;

fn raised(severity: Severity) -> Severity {
    match severity {
        Severity::Low => Severity::Medium,
        Severity::Medium => Severity::High,
        _ => Severity::Critical,
    }
}

impl Detector for DuplicatedCode {
    fn id(&self) -> &'static str {
        "duplicated_code"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::Duplication
    }
    fn summary(&self) -> &'static str {
        "Functions copied almost unchanged to other places."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let Some(duplicates) = context.duplicates else {
            return Vec::new();
        };
        if context.test {
            return Vec::new();
        }
        let c = &config.duplicated_code;
        duplicates
            .iter()
            .map(|d| {
                let places = d.copies.len() as u32 + 1;
                let mut severity = step(d.nloc, Severity::Low, &[(c.medium_nloc, Severity::Medium), (c.high_nloc, Severity::High)]);
                if places >= c.many_copies {
                    severity = raised(severity);
                }
                let shown: Vec<String> = d
                    .copies
                    .iter()
                    .take(3)
                    .map(|s| format!("{}:{}", s.path, s.line))
                    .collect();
                let more = d.copies.len().saturating_sub(3);
                let mut draft = Draft {
                    severity,
                    function: None,
                    class: None,
                    lines: (d.start_line, d.end_line),
                    measured: vec![("nloc", d.nloc), ("copies", places - 1)],
                    reason: format!(
                        "{} ({} lines) is a near copy of the code at {}{}; extract the shared logic so a fix is made once.",
                        code(&d.function),
                        d.nloc,
                        shown.join(", "),
                        if more > 0 { format!(" and {more} more") } else { String::new() },
                    ),
                };
                draft.class = Some(d.function.clone());
                finding(self, context, draft)
            })
            .collect()
    }
}

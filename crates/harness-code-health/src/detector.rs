use std::collections::BTreeMap;

use harness_code_analysis::metrics::FunctionMetrics;

use crate::{
    config::DetectorConfig,
    context::FileContext,
    model::{Category, Finding, Severity},
};

pub trait Detector: Send + Sync {
    /// snake_case, stable: findings and configuration refer to it.
    fn id(&self) -> &'static str;
    /// Bumped when what the detector reports changes.
    fn version(&self) -> u32;
    fn category(&self) -> Category;
    /// What it looks for, in a sentence.
    fn summary(&self) -> &'static str;
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding>;
}

/// The highest step `value` reaches: `steps` are `(minimum, severity)` from low to high.
pub(crate) fn step(value: u32, base: Severity, steps: &[(u32, Severity)]) -> Severity {
    steps
        .iter()
        .filter(|(minimum, _)| value >= *minimum)
        .map(|(_, severity)| *severity)
        .max()
        .unwrap_or(base)
        .max(base)
}

pub(crate) struct Draft<'a> {
    pub severity: Severity,
    pub function: Option<&'a FunctionMetrics>,
    pub class: Option<String>,
    pub lines: (u32, u32),
    pub measured: Vec<(&'static str, u32)>,
    pub reason: String,
}

pub(crate) fn finding(
    detector: &dyn Detector,
    context: &FileContext<'_>,
    draft: Draft<'_>,
) -> Finding {
    let name = draft
        .function
        .map(|f| f.qualified_name.clone())
        .or_else(|| draft.class.clone())
        .unwrap_or_default();
    Finding {
        id: format!(
            "{}:{}:{}:{}",
            detector.id(),
            context.path,
            draft.lines.0,
            name
        ),
        detector: detector.id().to_owned(),
        detector_version: detector.version(),
        category: detector.category(),
        severity: draft.severity,
        path: context.path.to_owned(),
        function: draft.function.map(|f| f.qualified_name.clone()),
        class: draft.class,
        start_line: draft.lines.0,
        end_line: draft.lines.1,
        measured: draft
            .measured
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
        reason: draft.reason,
    }
}

/// A finding about the whole file, as history findings are.
pub(crate) fn file_draft(
    severity: Severity,
    measured: Vec<(&'static str, u32)>,
    reason: String,
) -> Draft<'static> {
    Draft {
        severity,
        function: None,
        class: None,
        lines: (1, 1),
        measured,
        reason,
    }
}

/// Thousandths as a percentage, for reasons.
pub(crate) fn percent(per_mille: u32) -> String {
    format!("{}%", per_mille / 10)
}

/// `name` in backticks, as reasons write it.
pub(crate) fn code(name: &str) -> String {
    format!("`{name}`")
}

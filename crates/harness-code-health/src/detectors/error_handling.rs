use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Failures that are caught and then ignored: empty handlers, catch-alls, thrown-away results.
pub struct SwallowedError;

impl Detector for SwallowedError {
    fn id(&self) -> &'static str {
        "swallowed_error"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::ErrorHandling
    }
    fn summary(&self) -> &'static str {
        "Errors that are caught or discarded without being handled."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        if context.test {
            return Vec::new();
        }
        let c = &config.swallowed_error;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| !f.test_code)
            .filter_map(|f| {
                let e = &f.errors;
                // A discarded result is the weakest of the three: it takes two to count as one.
                let total = e.empty_handlers + e.broad_handlers + e.ignored_results / 2;
                (total > 0).then(|| {
                    let severity = step(total, Severity::Low, &[(c.medium, Severity::Medium), (c.high, Severity::High)]);
                    finding(self, context, Draft {
                        severity,
                        function: Some(f),
                        class: None,
                        lines: (f.start_line, f.end_line),
                        measured: vec![("empty_handlers", e.empty_handlers), ("broad_handlers", e.broad_handlers), ("ignored_results", e.ignored_results)],
                        reason: format!(
                            "{} swallows {} error{} ({} empty handler{}, {} catch-all, {} ignored result{}); handle it, log it with context, or let it propagate.",
                            code(&f.qualified_name),
                            total,
                            if total == 1 { "" } else { "s" },
                            e.empty_handlers,
                            if e.empty_handlers == 1 { "" } else { "s" },
                            e.broad_handlers,
                            e.ignored_results,
                            if e.ignored_results == 1 { "" } else { "s" },
                        ),
                    })
                })
            })
            .collect()
    }
}

/// Functions that abort instead of handling: unwrap, expect, panic, `!!`, printStackTrace.
pub struct PanicProne;

impl Detector for PanicProne {
    fn id(&self) -> &'static str {
        "panic_prone"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::ErrorHandling
    }
    fn summary(&self) -> &'static str {
        "Functions that crash or dump on failure instead of returning an error."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        if context.test {
            return Vec::new();
        }
        let c = &config.panic_prone;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| !f.test_code && f.errors.panics >= c.threshold)
            .map(|f| {
                let n = f.errors.panics;
                let severity = step(n, Severity::Low, &[(c.medium, Severity::Medium), (c.high, Severity::High)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("panics", n), ("limit", c.threshold)],
                    reason: format!("{} has {n} calls that abort on failure (limit {}); return an error and let the caller decide.", code(&f.qualified_name), c.threshold),
                })
            })
            .collect()
    }
}

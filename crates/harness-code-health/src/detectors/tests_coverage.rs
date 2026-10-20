use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, file_draft, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Code of real size that nothing in the repository tests.
pub struct UntestedCode;

impl Detector for UntestedCode {
    fn id(&self) -> &'static str {
        "untested_code"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::TestCoverage
    }
    fn summary(&self) -> &'static str {
        "Sizeable files with no test that imports them, is named after them, or sits in them."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let (Some(links), false) = (context.tests, context.test) else {
            return Vec::new();
        };
        let c = &config.untested_code;
        if links.repo_tests < c.min_repo_tests || links.linked() {
            return Vec::new();
        }
        let m = context.metrics;
        let ccn: u32 = m.functions.iter().map(|f| f.ccn.saturating_sub(1)).sum();
        if m.functions.is_empty() || (m.nloc < c.min_nloc && ccn < c.min_ccn) {
            return Vec::new();
        }
        let busy = match (context.importers, context.repo_importers_p80) {
            (Some(i), Some(p80)) => i > 0 && i >= p80,
            _ => false,
        };
        let hot = context.history.is_some_and(|h| h.hotspot);
        let severity = if busy && hot {
            Severity::High
        } else if busy || hot {
            Severity::Medium
        } else {
            Severity::Low
        };
        let who = match (busy, hot) {
            (true, true) => " It is imported widely and changes often.",
            (true, false) => " Many files import it.",
            (false, true) => " It changes often.",
            _ => "",
        };
        vec![finding(
            self,
            context,
            file_draft(
                severity,
                vec![("nloc", m.nloc), ("branching", ccn), ("test_files", links.repo_tests)],
                format!("{} lines with no test that imports it, is named after it or lives in it.{who} Add tests for its main paths.", m.nloc),
            ),
        )]
    }
}

/// Tests that run code and check nothing.
pub struct TestWithoutAssertions;

impl Detector for TestWithoutAssertions {
    fn id(&self) -> &'static str {
        "test_without_assertions"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::TestQuality
    }
    fn summary(&self) -> &'static str {
        "Test functions with no assertion."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.test_without_assertions;
        context
            .metrics
            .functions
            .iter()
            .filter(|f| f.test && f.assertions == 0 && f.nloc >= c.min_nloc)
            .map(|f| {
                let severity = step(f.nloc, Severity::Low, &[(c.medium_nloc, Severity::Medium)]);
                finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.end_line),
                    measured: vec![("nloc", f.nloc)],
                    reason: format!("Test {} runs {} lines but asserts nothing, so it passes whatever the code does; assert the result.", code(&f.qualified_name), f.nloc),
                })
            })
            .collect()
    }
}

/// Tests that are switched off.
pub struct SkippedTests;

impl Detector for SkippedTests {
    fn id(&self) -> &'static str {
        "skipped_tests"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::TestQuality
    }
    fn summary(&self) -> &'static str {
        "Files with several ignored or skipped tests."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.skipped_tests;
        let n = context.metrics.skipped_tests;
        if n < c.threshold {
            return Vec::new();
        }
        let severity = step(n, Severity::Low, &[(c.medium, Severity::Medium)]);
        vec![finding(
            self,
            context,
            file_draft(
                severity,
                vec![("skipped", n), ("limit", c.threshold)],
                format!("{n} tests are skipped or ignored (limit {}); fix or delete them so the suite means what it says.", c.threshold),
            ),
        )]
    }
}

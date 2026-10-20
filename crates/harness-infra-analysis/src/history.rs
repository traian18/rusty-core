//! How an infrastructure file has been changed: the code-health history detectors (churn,
//! ownership, change scatter, prior fixes, ...) run on a file that has no functions or classes.
use harness_code_analysis::{metrics::FileMetrics, LanguageId};
use harness_code_health::{detect_file, DetectorConfig, FileContext, Finding};
use harness_git_analysis::{FileSignals, SignalSummary};

/// History findings for `path`, whose size is `nloc` lines of code.
///
/// The structural detectors find no functions to judge and stay silent. The language in the
/// placeholder measurements is not read by any history detector.
pub fn history_findings(
    path: &str,
    nloc: u32,
    signals: &FileSignals,
    summary: &SignalSummary,
) -> Vec<Finding> {
    let metrics = FileMetrics {
        language: LanguageId::Go,
        nloc,
        partial: false,
        functions: Vec::new(),
        classes: Vec::new(),
        skipped_tests: 0,
    };
    let context = FileContext::new(path, &metrics).with_history(signals, summary);
    detect_file(&context, &DetectorConfig::default())
}

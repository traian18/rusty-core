use harness_code_analysis::metrics::FileMetrics;
use harness_git_analysis::{FileSignals, SignalSummary};

/// What a detector knows about one file. Signals that are unknown stay `None`; a detector that
/// needs one stays silent rather than assuming a value.
#[derive(Clone, Copy, Debug)]
pub struct FileContext<'a> {
    /// Repository-relative path.
    pub path: &'a str,
    pub metrics: &'a FileMetrics,
    /// Files that import this one.
    pub importers: Option<u32>,
    /// The 80th percentile of importers per file across the repository.
    pub repo_importers_p80: Option<u32>,
    /// How the file has been changed, from the Git analysis.
    pub history: Option<&'a FileSignals>,
    /// The windows the history was computed over, and repository-wide counts.
    pub history_summary: Option<&'a SignalSummary>,
    /// A test file, by the caller's convention.
    pub test: bool,
}

impl<'a> FileContext<'a> {
    pub fn new(path: &'a str, metrics: &'a FileMetrics) -> Self {
        Self {
            path,
            metrics,
            importers: None,
            repo_importers_p80: None,
            history: None,
            history_summary: None,
            test: false,
        }
    }

    pub fn with_importers(mut self, importers: u32, repo_p80: Option<u32>) -> Self {
        self.importers = Some(importers);
        self.repo_importers_p80 = repo_p80;
        self
    }

    pub fn with_history(mut self, signals: &'a FileSignals, summary: &'a SignalSummary) -> Self {
        self.history = Some(signals);
        self.history_summary = Some(summary);
        self
    }

    pub fn with_test(mut self, test: bool) -> Self {
        self.test = test;
        self
    }
}

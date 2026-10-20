use harness_code_analysis::metrics::FileMetrics;
use harness_git_analysis::{FileSignals, SignalSummary};

/// Another place a function is copied to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateSite {
    pub path: String,
    pub function: String,
    pub line: u32,
}

/// A function that has near copies elsewhere in the repository (same syntax tree shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub function: String,
    pub start_line: u32,
    pub end_line: u32,
    pub nloc: u32,
    /// The other places, not this one.
    pub copies: Vec<DuplicateSite>,
}

/// What links a file to tests, from the import graph, naming and the file itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestLinks {
    /// Test files that import the file.
    pub test_files: u32,
    /// Test files that reach it through one intermediate file.
    pub indirect: u32,
    /// A test file named after it sits next to it or in a test folder.
    pub sibling: bool,
    /// The file holds its own tests.
    pub in_file: bool,
    /// Test files in the whole repository.
    pub repo_tests: u32,
}

impl TestLinks {
    pub fn linked(&self) -> bool {
        self.test_files > 0 || self.indirect > 0 || self.sibling || self.in_file
    }
}

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
    /// Near copies of this file's functions elsewhere, when the caller compared them.
    pub duplicates: Option<&'a [Duplicate]>,
    /// What links the file to tests, when the caller worked it out.
    pub tests: Option<&'a TestLinks>,
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
            duplicates: None,
            tests: None,
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

    pub fn with_duplicates(mut self, duplicates: &'a [Duplicate]) -> Self {
        self.duplicates = Some(duplicates);
        self
    }

    pub fn with_tests(mut self, tests: &'a TestLinks) -> Self {
        self.tests = Some(tests);
        self
    }

    pub fn with_test(mut self, test: bool) -> Self {
        self.test = test;
        self
    }
}

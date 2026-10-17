#![warn(clippy::all)]

//! Findings: ranked, explainable observations derived from the repository graph, dependency
//! groups, and Git analysis.
//!
//! A finding is derived data, separate from graph entities and edges. It carries a rule id and
//! version, severity, confidence, evidence, source locations, and a rationale. Measurable
//! findings come from facts; candidates are labelled heuristics. There are no health scores:
//! findings rank by kind, severity, confidence, then impact.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model`, `config`, `error` | Finding data, thresholds/policy/suppressions, failures |
//! | `view` | Indexed read-only lookups over one repository snapshot |
//! | `rules` | One versioned rule per finding type |
//! | `source` | Source reader and byte-span to line/column resolution |
//! | `analyzer` | Runs rules; bounds, suppresses, locates, and ranks |
//! | `set`, `service` | Immutable finding sets, paged queries, and the consumer API |
mod analyzer;
mod config;
mod error;
mod model;
mod rules;
mod service;
mod set;
mod source;
mod view;

pub use analyzer::{AnalysisInput, DefaultFindingsAnalyzer, FindingsAnalyzer};
pub use config::{BoundaryPolicy, BoundaryRule, FindingsConfig, Selector, Suppression};
pub use error::{FindingsError, FindingsQueryError};
pub use model::{
    Confidence, Draft, Evidence, Finding, FindingId, FindingKind, GitEvidence, Location, RuleRef,
    Severity, SourceSpan, CANDIDATE_INTERPRETATION,
};
pub use rules::{
    BoundaryViolationRule, ChangeHotspotRule, FindingRule, FindingsInput, ImplicitCouplingRule,
    ImportCycleRule, Requirements, TypeOnlyImportCycleRule, UnattachedFileRule,
    UndeclaredDependencyRule, UndeclaredWorkspaceDependencyRule, UnreferencedFileRule,
    UnusedDeclaredDependencyRule,
};
pub use service::{AnalyzeRequest, FindingExplanation, FindingsAnalysis, FindingsService};
pub use set::{
    FindingEntry, FindingSet, FindingSetId, FindingsCursor, FindingsListQuery, FindingsPage,
    FindingsSummary, SkippedRule, SuppressedFinding,
};
pub use source::{FsSourceTextReader, Locator, SourceTextReader};
pub use view::{RepoView, Walk};

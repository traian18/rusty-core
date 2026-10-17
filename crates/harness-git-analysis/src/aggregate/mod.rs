//! Turns normalized commits into churn, co-change, and contributor statistics.
//!
//! Everything here is a pure function of the commit list and the configuration, so a
//! snapshot built from retained commits equals one built from a fresh read.
mod renames;
mod tally;

use std::collections::BTreeSet;

use harness_workspace::repository_intelligence::RootRelativePath;

use crate::{
    config::GitAnalysisConfig, error::GitAnalysisError, exclusions::ExclusionSet,
    model::CommitFact, snapshot::GitIndex,
};

/// Counts the analysis keeps besides the index itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AggregationCounts {
    pub commits_used: usize,
    pub merges_skipped: usize,
    pub bulk_commits_excluded: usize,
    pub excluded_changes: usize,
    pub changes_without_lines: usize,
}

pub struct Aggregation {
    pub index: GitIndex,
    pub counts: AggregationCounts,
}

/// `present_at_head` reports which of the given files still exist at `HEAD`.
pub fn aggregate(
    commits: &[CommitFact],
    config: &GitAnalysisConfig,
    present_at_head: impl FnOnce(
        &[RootRelativePath],
    ) -> Result<BTreeSet<RootRelativePath>, GitAnalysisError>,
) -> Result<Aggregation, GitAnalysisError> {
    let exclusions = ExclusionSet::new(&config.exclusions);
    let paths = renames::head_paths(commits);
    let mut tally = tally::Tally::new(config);
    for (commit, head_paths) in commits.iter().zip(&paths) {
        tally.add_commit(commit, head_paths, &exclusions);
    }
    tally.finish(present_at_head)
}

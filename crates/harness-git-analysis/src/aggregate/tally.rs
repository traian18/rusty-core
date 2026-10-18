//! Running totals while commits are folded in newest first.
use std::collections::{BTreeMap, BTreeSet, HashMap};

use harness_workspace::repository_intelligence::RootRelativePath;

use super::{Aggregation, AggregationCounts};
use crate::{
    config::GitAnalysisConfig,
    error::GitAnalysisError,
    exclusions::ExclusionSet,
    model::{CommitFact, CommitId, Contributor, ContributorId},
    snapshot::{CoChange, ContributorShare, FileChurn, FileContributors, GitIndex},
};

struct ContributorTally {
    name: String,
    commits: u32,
    lines: u64,
}

struct FileTally {
    path: RootRelativePath,
    commits: u32,
    lines_added: u64,
    lines_removed: u64,
    changes_without_lines: u32,
    first_change: i64,
    last_change: i64,
    contributors: HashMap<ContributorId, ContributorTally>,
}

#[derive(Default)]
struct PairTally {
    support: u32,
    examples: Vec<CommitId>,
}

/// One commit's effect on one file.
#[derive(Default)]
struct FileDelta {
    added: u64,
    removed: u64,
    without_lines: u32,
}

pub(super) struct Tally<'c> {
    config: &'c GitAnalysisConfig,
    ids: HashMap<RootRelativePath, u32>,
    files: Vec<FileTally>,
    pairs: HashMap<(u32, u32), PairTally>,
    counts: AggregationCounts,
}

impl<'c> Tally<'c> {
    pub fn new(config: &'c GitAnalysisConfig) -> Self {
        Self {
            config,
            ids: HashMap::new(),
            files: Vec::new(),
            pairs: HashMap::new(),
            counts: AggregationCounts::default(),
        }
    }

    pub fn add_commit(
        &mut self,
        commit: &CommitFact,
        head_paths: &[RootRelativePath],
        exclusions: &ExclusionSet,
    ) {
        if commit.is_merge() {
            self.counts.merges_skipped += 1;
            return;
        }
        let touched = self.analysed_files(commit, head_paths, exclusions);
        if touched.is_empty() {
            return;
        }
        self.counts.commits_used += 1;
        let ids = touched
            .iter()
            .map(|(path, delta)| self.record_file(commit, path, delta))
            .collect::<Vec<_>>();
        self.record_pairs(commit, ids);
    }

    /// The commit's distinct, non-excluded files at their `HEAD` paths.
    fn analysed_files(
        &mut self,
        commit: &CommitFact,
        head_paths: &[RootRelativePath],
        exclusions: &ExclusionSet,
    ) -> BTreeMap<RootRelativePath, FileDelta> {
        let mut touched: BTreeMap<RootRelativePath, FileDelta> = BTreeMap::new();
        for (change, head_path) in commit.changes.iter().zip(head_paths) {
            if exclusions.excludes(head_path) || exclusions.excludes(&change.path) {
                self.counts.excluded_changes += 1;
                continue;
            }
            let delta = touched.entry(head_path.clone()).or_default();
            match (change.lines_added, change.lines_removed) {
                (Some(added), Some(removed)) => {
                    delta.added += u64::from(added);
                    delta.removed += u64::from(removed);
                }
                _ => delta.without_lines += 1,
            }
        }
        touched
    }

    fn record_file(
        &mut self,
        commit: &CommitFact,
        path: &RootRelativePath,
        delta: &FileDelta,
    ) -> u32 {
        let next = self.files.len() as u32;
        let id = *self.ids.entry(path.clone()).or_insert(next);
        if id == next {
            self.files.push(FileTally {
                path: path.clone(),
                commits: 0,
                lines_added: 0,
                lines_removed: 0,
                changes_without_lines: 0,
                first_change: commit.author_time,
                last_change: commit.author_time,
                contributors: HashMap::new(),
            });
        }
        let file = &mut self.files[id as usize];
        file.commits += 1;
        file.lines_added += delta.added;
        file.lines_removed += delta.removed;
        file.changes_without_lines += delta.without_lines;
        file.first_change = file.first_change.min(commit.author_time);
        file.last_change = file.last_change.max(commit.author_time);
        let contributor = file
            .contributors
            .entry(commit.contributor.id.clone())
            .or_insert_with(|| ContributorTally {
                name: commit.contributor.name.clone(),
                commits: 0,
                lines: 0,
            });
        contributor.commits += 1;
        contributor.lines += delta.added + delta.removed;
        self.counts.changes_without_lines += delta.without_lines as usize;
        id
    }

    fn record_pairs(&mut self, commit: &CommitFact, mut ids: Vec<u32>) {
        if ids.len() > self.config.max_files_per_commit {
            self.counts.bulk_commits_excluded += 1;
            return;
        }
        ids.sort_unstable();
        for (position, &first) in ids.iter().enumerate() {
            for &second in &ids[position + 1..] {
                let pair = self.pairs.entry((first, second)).or_default();
                pair.support += 1;
                if pair.examples.len() < self.config.max_examples {
                    pair.examples.push(commit.id.clone());
                }
            }
        }
    }

    pub fn finish(
        self,
        present_at_head: impl FnOnce(
            &[RootRelativePath],
        ) -> Result<BTreeSet<RootRelativePath>, GitAnalysisError>,
    ) -> Result<Aggregation, GitAnalysisError> {
        let co_changes = self.co_changes();
        let mut order = (0..self.files.len()).collect::<Vec<_>>();
        order.sort_by(|&a, &b| self.files[a].path.cmp(&self.files[b].path));
        let all_paths = order
            .iter()
            .map(|&i| self.files[i].path.clone())
            .collect::<Vec<_>>();
        let present = present_at_head(&all_paths)?;
        let churn = order
            .iter()
            .map(|&i| {
                let file = &self.files[i];
                FileChurn {
                    file: file.path.clone(),
                    commits: file.commits,
                    lines_added: file.lines_added,
                    lines_removed: file.lines_removed,
                    changes_without_lines: file.changes_without_lines,
                    contributors: file.contributors.len() as u32,
                    first_change: file.first_change,
                    last_change: file.last_change,
                    absent_at_head: !present.contains(&file.path),
                }
            })
            .collect();
        let contributors = order
            .iter()
            .map(|&i| contributor_shares(&self.files[i]))
            .collect();
        Ok(Aggregation {
            index: GitIndex {
                churn,
                co_changes,
                contributors,
                ..GitIndex::default()
            },
            counts: self.counts,
        })
    }

    /// Strongest partners per file, from pairs meeting the support and confidence thresholds.
    fn co_changes(&self) -> Vec<CoChange> {
        let mut candidates: HashMap<u32, Vec<(u32, u32, u32)>> = HashMap::new();
        for (&(first, second), pair) in &self.pairs {
            if pair.support < self.config.min_support {
                continue;
            }
            for (file, partner) in [(first, second), (second, first)] {
                let confidence = pair.support * 1000 / self.files[file as usize].commits;
                if confidence >= self.config.min_confidence_per_mille {
                    candidates
                        .entry(file)
                        .or_default()
                        .push((partner, pair.support, confidence));
                }
            }
        }
        let mut result = Vec::new();
        let mut files = candidates.into_iter().collect::<Vec<_>>();
        files.sort_by(|a, b| {
            self.files[a.0 as usize]
                .path
                .cmp(&self.files[b.0 as usize].path)
        });
        for (file, mut partners) in files {
            partners.sort_by(|a, b| {
                b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(
                    self.files[a.0 as usize]
                        .path
                        .cmp(&self.files[b.0 as usize].path),
                )
            });
            partners.truncate(self.config.max_partners_per_file);
            for (partner, support, confidence) in partners {
                let key = (file.min(partner), file.max(partner));
                result.push(CoChange {
                    file: self.files[file as usize].path.clone(),
                    partner: self.files[partner as usize].path.clone(),
                    support,
                    file_commits: self.files[file as usize].commits,
                    confidence_per_mille: confidence,
                    example_commits: self.pairs[&key].examples.clone(),
                });
            }
        }
        result
    }
}

fn contributor_shares(file: &FileTally) -> FileContributors {
    let total_lines: u64 = file.contributors.values().map(|c| c.lines).sum();
    let mut entries = file
        .contributors
        .iter()
        .map(|(id, tally)| ContributorShare {
            contributor: Contributor {
                id: id.clone(),
                name: tally.name.clone(),
            },
            commits: tally.commits,
            lines_changed: tally.lines,
            commit_share_per_mille: tally.commits * 1000 / file.commits,
            line_share_per_mille: (tally.lines * 1000).checked_div(total_lines).unwrap_or(0) as u32,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.commits
            .cmp(&a.commits)
            .then(a.contributor.id.cmp(&b.contributor.id))
    });
    FileContributors {
        file: file.path.clone(),
        entries,
    }
}

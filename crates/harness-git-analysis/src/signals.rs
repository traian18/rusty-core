//! Per-file signals over time windows, for code health: recent activity, bug-fix history,
//! time-decayed churn and the hotspot rule, change entropy, and ownership.
//!
//! Every window is measured back from the newest commit analysed, not from the clock, so the same
//! history always gives the same signals. All values are integers (thousandths where a fraction is
//! meant) so snapshots stay comparable byte for byte.
//!
//! Definitions:
//! - **Recent**: the 90 days before the newest commit. **Fix window**: the 180 days before it.
//! - **Decayed churn**: for each commit, lines added plus removed divided by 100, halved for every
//!   180 days of age; summed per file. One 50-line change today scores 0.5.
//! - **Hotspot**: churn percentile at least 75% (top quartile of decayed churn among files with
//!   history) and at least 3 recent commits and either decayed churn of at least 0.5 or at least 8
//!   recent commits. The absolute floors keep a quiet repository's "top quartile" from meaning
//!   "touched once".
//! - **Change entropy** (after Hassan): for each commit touching 2 to 30 analysed files, the
//!   normalised Shannon entropy of how its changed lines spread over those files (1.0 = evenly);
//!   a file's entropy is the sum over the commits that touched it. A file changed mostly in
//!   scattered commits scores high; a file changed on its own scores zero.
//! - **Ownership**: the main author and their share of the file's commits, the main author of
//!   the recent window, minor contributors (under 5% of commits each), and the bus factor (the
//!   fewest authors who made at least half of the commits).
//! - **Co-change partners**: files changed together with this one in at least two commits of 2
//!   to 30 files (the same commits entropy reads), and their percentile.
//! - **Active contributors**: authors of any analysed commit in the recent window.
//! - **Percentiles**: the share of files with history whose value is lower, in thousandths.
use std::collections::{BTreeMap, HashMap};

use harness_workspace::repository_intelligence::RootRelativePath;
use serde::{Deserialize, Serialize};

use crate::{
    exclusions::ExclusionSet,
    fix_rule::FIX_RULE_VERSION,
    model::{CommitFact, CommitId, Contributor, ContributorId},
};

/// Bumped whenever a definition above changes.
pub const SIGNALS_VERSION: u32 = 1;

const DAY: i64 = 86_400;
pub const RECENT_DAYS: i64 = 90;
pub const FIX_WINDOW_DAYS: i64 = 180;
const HALF_LIFE_DAYS: f64 = 180.0;
const LINES_PER_POINT: f64 = 100.0;
const HOTSPOT_PERCENTILE: u32 = 750;
const HOTSPOT_MIN_RECENT_COMMITS: u32 = 3;
const HOTSPOT_MIN_DECAYED: u32 = 500;
const HOTSPOT_HIGH_RECENT_COMMITS: u32 = 8;
const ENTROPY_MAX_FILES: usize = 30;
const MINOR_SHARE: u32 = 50;
const FIX_EXAMPLES: usize = 3;
/// Shared commits that make two files co-change partners.
const MIN_PARTNER_COMMITS: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileSignals {
    pub file: RootRelativePath,
    pub commits: u32,
    pub commits_recent: u32,
    pub lines_changed_recent: u64,
    /// Bug-fix commits in the fix window.
    pub fix_commits: u32,
    /// The newest fix commits, at most three.
    pub fix_examples: Vec<CommitId>,
    /// Decayed churn in thousandths of a point.
    pub decayed_churn_milli: u32,
    pub churn_percentile_per_mille: u32,
    pub hotspot: bool,
    /// Change entropy in thousandths.
    pub change_entropy_milli: u32,
    pub change_entropy_percentile_per_mille: u32,
    pub contributors: u32,
    pub owner: Option<Contributor>,
    pub owner_share_per_mille: u32,
    pub recent_owner: Option<Contributor>,
    pub recent_owner_share_per_mille: u32,
    pub minor_contributors: u32,
    pub bus_factor: u32,
    /// Days from the file's first change in the window to the newest commit.
    pub age_days: u32,
    /// Days from the file's last change to the newest commit.
    pub idle_days: u32,
    /// Share of files with history that have fewer fix commits, in thousandths.
    pub fix_percentile_per_mille: u32,
    /// Authors in the recent window.
    pub recent_contributors: u32,
    /// The main author's share of the recent commits (0 when they made none).
    pub owner_recent_share_per_mille: u32,
    /// Files changed together with this one in at least two commits of 2 to 30 files.
    pub co_change_partners: u32,
    pub co_change_percentile_per_mille: u32,
}

/// The windows and rules the signals were computed with.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignalSummary {
    pub version: u32,
    pub fix_rule_version: u32,
    /// Seconds since the epoch of the newest commit; the windows end here.
    pub as_of: Option<i64>,
    pub recent_days: u32,
    pub fix_window_days: u32,
    pub hotspots: u32,
    /// Authors of any analysed commit in the recent window, across the repository.
    pub active_contributors_recent: u32,
    pub fix_commits: u32,
}

#[derive(Default)]
struct Tally {
    commits: u32,
    commits_recent: u32,
    lines_recent: u64,
    fixes: u32,
    fix_examples: Vec<CommitId>,
    decayed: f64,
    entropy: f64,
    first: i64,
    last: i64,
    authors: HashMap<ContributorId, (String, u32)>,
    recent_authors: HashMap<ContributorId, (String, u32)>,
    partners: HashMap<RootRelativePath, u32>,
}

fn percentiles(values: &[u32]) -> Vec<u32> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = values.len().max(1);
    values
        .iter()
        .map(|v| {
            if *v == 0 {
                0
            } else {
                (sorted.partition_point(|x| x < v) * 1000 / n) as u32
            }
        })
        .collect()
}

/// The author with most commits (ties go to the smaller id, for determinism) and their share.
fn top(authors: &HashMap<ContributorId, (String, u32)>, total: u32) -> (Option<Contributor>, u32) {
    authors
        .iter()
        .max_by(|a, b| a.1 .1.cmp(&b.1 .1).then(b.0.cmp(a.0)))
        .map(|(id, (name, commits))| {
            (
                Some(Contributor {
                    id: id.clone(),
                    name: name.clone(),
                }),
                commits * 1000 / total.max(1),
            )
        })
        .unwrap_or((None, 0))
}

fn bus_factor(authors: &HashMap<ContributorId, (String, u32)>, total: u32) -> u32 {
    let mut counts: Vec<u32> = authors.values().map(|(_, c)| *c).collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    let mut covered = 0;
    for (index, count) in counts.iter().enumerate() {
        covered += count;
        if covered * 2 >= total {
            return index as u32 + 1;
        }
    }
    counts.len() as u32
}

/// Signals for every analysed file, sorted by file. `commits` are newest first and `head_paths`
/// gives each change's path at `HEAD`, as for the other aggregates.
pub fn compute(
    commits: &[CommitFact],
    head_paths: &[Vec<RootRelativePath>],
    exclusions: &ExclusionSet,
) -> (Vec<FileSignals>, SignalSummary) {
    let as_of = commits.iter().map(|c| c.author_time).max();
    let Some(now) = as_of else {
        return (
            Vec::new(),
            SignalSummary {
                version: SIGNALS_VERSION,
                fix_rule_version: FIX_RULE_VERSION,
                ..Default::default()
            },
        );
    };
    let mut tallies: BTreeMap<RootRelativePath, Tally> = BTreeMap::new();
    let mut fix_total = 0;
    let mut active: std::collections::BTreeSet<ContributorId> = std::collections::BTreeSet::new();
    for (commit, paths) in commits.iter().zip(head_paths) {
        if commit.is_merge() {
            continue;
        }
        // Lines per analysed file in this commit, at HEAD paths.
        let mut touched: BTreeMap<&RootRelativePath, u64> = BTreeMap::new();
        for (change, head) in commit.changes.iter().zip(paths) {
            if exclusions.excludes(head) || exclusions.excludes(&change.path) {
                continue;
            }
            let lines = u64::from(change.lines_added.unwrap_or(0))
                + u64::from(change.lines_removed.unwrap_or(0));
            *touched.entry(head).or_default() += lines;
        }
        if touched.is_empty() {
            continue;
        }
        let age = (now - commit.author_time).max(0);
        let recent = age <= RECENT_DAYS * DAY;
        let fix = age <= FIX_WINDOW_DAYS * DAY && commit.is_fix();
        fix_total += u32::from(fix);
        let weight = 0.5_f64.powf(age as f64 / DAY as f64 / HALF_LIFE_DAYS);
        let entropy = if (2..=ENTROPY_MAX_FILES).contains(&touched.len()) {
            let total: u64 = touched.values().sum();
            let n = touched.len() as f64;
            if total == 0 {
                1.0
            } else {
                -touched
                    .values()
                    .filter(|l| **l > 0)
                    .map(|l| {
                        let p = *l as f64 / total as f64;
                        p * p.log2()
                    })
                    .sum::<f64>()
                    / n.log2()
            }
        } else {
            0.0
        };
        if recent {
            active.insert(commit.contributor.id.clone());
        }
        let together: Vec<RootRelativePath> = if (2..=ENTROPY_MAX_FILES).contains(&touched.len()) {
            touched.keys().map(|p| (*p).clone()).collect()
        } else {
            Vec::new()
        };
        for (path, lines) in touched {
            let t = tallies.entry(path.clone()).or_insert_with(|| Tally {
                first: commit.author_time,
                last: commit.author_time,
                ..Default::default()
            });
            t.commits += 1;
            t.first = t.first.min(commit.author_time);
            t.last = t.last.max(commit.author_time);
            t.decayed += weight * lines as f64 / LINES_PER_POINT;
            t.entropy += entropy;
            for partner in together.iter().filter(|p| *p != path) {
                *t.partners.entry(partner.clone()).or_default() += 1;
            }
            let author = (
                commit.contributor.id.clone(),
                commit.contributor.name.clone(),
            );
            // Commits arrive newest first, so the first name seen is the newest spelling.
            t.authors
                .entry(author.0.clone())
                .or_insert((author.1.clone(), 0))
                .1 += 1;
            if recent {
                t.commits_recent += 1;
                t.lines_recent += lines;
                t.recent_authors.entry(author.0).or_insert((author.1, 0)).1 += 1;
            }
            if fix {
                t.fixes += 1;
                if t.fix_examples.len() < FIX_EXAMPLES {
                    t.fix_examples.push(commit.id.clone());
                }
            }
        }
    }
    let decayed: Vec<u32> = tallies
        .values()
        .map(|t| (t.decayed * 1000.0).round() as u32)
        .collect();
    let entropy: Vec<u32> = tallies
        .values()
        .map(|t| (t.entropy * 1000.0).round() as u32)
        .collect();
    let churn_pct = percentiles(&decayed);
    let entropy_pct = percentiles(&entropy);
    let fixes: Vec<u32> = tallies.values().map(|t| t.fixes).collect();
    let fix_pct = percentiles(&fixes);
    let partners: Vec<u32> = tallies
        .values()
        .map(|t| {
            t.partners
                .values()
                .filter(|n| **n >= MIN_PARTNER_COMMITS)
                .count() as u32
        })
        .collect();
    let partner_pct = percentiles(&partners);
    let mut hotspots = 0;
    let signals: Vec<FileSignals> = tallies
        .into_iter()
        .enumerate()
        .map(|(i, (file, t))| {
            let (owner, owner_share) = top(&t.authors, t.commits);
            let owner_recent = owner
                .as_ref()
                .and_then(|o| t.recent_authors.get(&o.id))
                .map_or(0, |(_, n)| n * 1000 / t.commits_recent.max(1));
            let (recent_owner, recent_share) = top(&t.recent_authors, t.commits_recent);
            let minor = t
                .authors
                .values()
                .filter(|(_, c)| c * 1000 / t.commits.max(1) < MINOR_SHARE)
                .count() as u32;
            let hotspot = churn_pct[i] >= HOTSPOT_PERCENTILE
                && t.commits_recent >= HOTSPOT_MIN_RECENT_COMMITS
                && (decayed[i] >= HOTSPOT_MIN_DECAYED
                    || t.commits_recent >= HOTSPOT_HIGH_RECENT_COMMITS);
            hotspots += u32::from(hotspot);
            FileSignals {
                file,
                commits: t.commits,
                commits_recent: t.commits_recent,
                lines_changed_recent: t.lines_recent,
                fix_commits: t.fixes,
                fix_examples: t.fix_examples,
                decayed_churn_milli: decayed[i],
                churn_percentile_per_mille: churn_pct[i],
                hotspot,
                change_entropy_milli: entropy[i],
                change_entropy_percentile_per_mille: entropy_pct[i],
                contributors: t.authors.len() as u32,
                owner,
                owner_share_per_mille: owner_share,
                recent_owner,
                recent_owner_share_per_mille: recent_share,
                minor_contributors: minor,
                bus_factor: bus_factor(&t.authors, t.commits),
                fix_percentile_per_mille: fix_pct[i],
                recent_contributors: t.recent_authors.len() as u32,
                owner_recent_share_per_mille: owner_recent,
                co_change_partners: partners[i],
                co_change_percentile_per_mille: partner_pct[i],
                age_days: ((now - t.first).max(0) / DAY) as u32,
                idle_days: ((now - t.last).max(0) / DAY) as u32,
            }
        })
        .collect();
    let summary = SignalSummary {
        version: SIGNALS_VERSION,
        fix_rule_version: FIX_RULE_VERSION,
        as_of,
        recent_days: RECENT_DAYS as u32,
        fix_window_days: FIX_WINDOW_DAYS as u32,
        hotspots,
        active_contributors_recent: active.len() as u32,
        fix_commits: fix_total,
    };
    (signals, summary)
}

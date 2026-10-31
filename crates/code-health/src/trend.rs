//! How a repository's health moves over time: one point per analysis (or per past commit when
//! history is rebuilt from Git), and the files whose score moved between two points.
//!
//! `maintainability` is the comparable measure: it counts only what the code itself shows (shape,
//! duplication, error handling), not Git history or tests that need the import graph, so a point
//! rebuilt from an old commit is comparable with one recorded today. File movers use it too.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    model::{Finding, Severity},
    rollup::{repository_health, FileHealth, RollupConfig},
    scoring::{SCORE_FLOOR, SCORE_MAX},
};

/// Points kept; the oldest go first.
pub const MAX_POINTS: usize = 400;
/// A file moves between points when its maintainability score changes by at least this much.
pub const MOVE_THRESHOLD: f64 = 0.5;
/// Files named as regressions and improvements per point.
pub const MOVERS: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileMove {
    pub path: String,
    pub from: f64,
    pub to: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeverityTotals {
    pub critical: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrendPoint {
    /// Milliseconds since the epoch: when it was recorded, or the commit's time when rebuilt.
    pub at_ms: u64,
    /// The commit or Git snapshot it describes.
    pub commit: Option<String>,
    /// Rebuilt from Git history rather than recorded at the time.
    pub backfilled: bool,
    /// Digest of the settings it was scored with; points with different digests are not strictly
    /// comparable.
    pub digest: String,
    /// The headline score (includes Git history when it was available).
    pub score: Option<f64>,
    /// Code only: `10 - structure deduction`, line-weighted.
    pub structure_score: Option<f64>,
    pub maintainability: Option<f64>,
    pub findings: u32,
    pub severities: SeverityTotals,
    pub scored_files: usize,
    pub nloc: u64,
    /// The most frequent findings: detector id and count.
    pub top_detectors: Vec<(String, u32)>,
    /// Files whose maintainability fell / rose since the previous recorded point.
    pub regressions: Vec<FileMove>,
    pub improvements: Vec<FileMove>,
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// What identifies and describes a point apart from the measurements.
#[derive(Clone, Debug, Default)]
pub struct PointMeta {
    pub at_ms: u64,
    pub commit: Option<String>,
    pub backfilled: bool,
    pub digest: String,
}

/// Maintainability score of each scored non-test file, for comparing two points.
pub fn file_scores(files: &[FileHealth]) -> BTreeMap<String, f64> {
    files
        .iter()
        .filter(|f| !f.test)
        .filter_map(|f| {
            let s = f.score.as_ref()?;
            let value = s.maintainability.score.clamp(SCORE_FLOOR, SCORE_MAX);
            Some((f.path.clone(), round2(value)))
        })
        .collect()
}

/// Files whose maintainability moved by at least [`MOVE_THRESHOLD`]: `(regressions, improvements)`, the
/// biggest moves first. Files present in only one of the two are not moves.
pub fn moves(
    before: &BTreeMap<String, f64>,
    after: &BTreeMap<String, f64>,
) -> (Vec<FileMove>, Vec<FileMove>) {
    let mut down = Vec::new();
    let mut up = Vec::new();
    for (path, to) in after {
        let Some(from) = before.get(path) else {
            continue;
        };
        let change = to - from;
        if change.abs() < MOVE_THRESHOLD {
            continue;
        }
        let entry = FileMove {
            path: path.clone(),
            from: *from,
            to: *to,
        };
        if change < 0.0 {
            down.push(entry);
        } else {
            up.push(entry);
        }
    }
    down.sort_by(|a, b| {
        (a.to - a.from)
            .total_cmp(&(b.to - b.from))
            .then(a.path.cmp(&b.path))
    });
    up.sort_by(|a, b| {
        (b.to - b.from)
            .total_cmp(&(a.to - a.from))
            .then(a.path.cmp(&b.path))
    });
    down.truncate(MOVERS);
    up.truncate(MOVERS);
    (down, up)
}

impl TrendPoint {
    /// A point from the files' health and their findings; regressions are filled in by
    /// [`TrendSeries::push`], which knows the previous point.
    pub fn from_files<'a>(
        files: &[FileHealth],
        findings: impl IntoIterator<Item = &'a Finding>,
        meta: PointMeta,
    ) -> Self {
        let health = repository_health(files, &RollupConfig::default());
        let mut severities = SeverityTotals::default();
        let mut by_detector: BTreeMap<&str, u32> = BTreeMap::new();
        let mut total = 0;
        for finding in findings {
            total += 1;
            *by_detector.entry(finding.detector.as_str()).or_default() += 1;
            match finding.severity {
                Severity::Critical => severities.critical += 1,
                Severity::High => severities.high += 1,
                Severity::Medium => severities.medium += 1,
                Severity::Low => severities.low += 1,
            }
        }
        let mut top: Vec<(String, u32)> = by_detector
            .into_iter()
            .map(|(d, n)| (d.to_owned(), n))
            .collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top.truncate(8);
        Self {
            at_ms: meta.at_ms,
            commit: meta.commit,
            backfilled: meta.backfilled,
            digest: meta.digest,
            score: health.average,
            structure_score: health
                .structure_average
                .map(|d| round2((SCORE_MAX - d).clamp(SCORE_FLOOR, SCORE_MAX))),
            maintainability: health.maintainability_average,
            findings: total,
            severities,
            scored_files: health.file_count,
            nloc: files
                .iter()
                .filter(|f| f.score.is_some())
                .map(|f| u64::from(f.nloc))
                .sum(),
            top_detectors: top,
            regressions: Vec::new(),
            improvements: Vec::new(),
        }
    }
}

/// Points in time order, and the file scores of the newest recorded one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrendSeries {
    pub points: Vec<TrendPoint>,
    /// Maintainability per file at the newest recorded point.
    #[serde(default)]
    pub last_files: BTreeMap<String, f64>,
}

impl TrendSeries {
    /// Adds `point` (replacing one for the same commit, digest and kind), computing what moved
    /// since the previous recorded point from `files`. Backfilled points carry no movers.
    pub fn push(&mut self, mut point: TrendPoint, files: Option<&BTreeMap<String, f64>>) {
        if let Some(now) = files.filter(|_| !point.backfilled) {
            let (down, up) = moves(&self.last_files, now);
            point.regressions = down;
            point.improvements = up;
            self.last_files = now.clone();
        }
        self.points.retain(|p| {
            !(p.commit.is_some()
                && p.commit == point.commit
                && p.digest == point.digest
                && p.backfilled == point.backfilled)
        });
        self.points.push(point);
        self.points.sort_by_key(|p| p.at_ms);
        let excess = self.points.len().saturating_sub(MAX_POINTS);
        self.points.drain(..excess);
    }

    pub fn latest(&self) -> Option<&TrendPoint> {
        self.points.last()
    }

    /// Commits already in the series, so a rebuild skips them.
    pub fn commits(&self, backfilled: bool) -> BTreeSet<&str> {
        self.points
            .iter()
            .filter(|p| p.backfilled == backfilled)
            .filter_map(|p| p.commit.as_deref())
            .collect()
    }

    /// The newest point at least `ms` older than the latest, to compare against ("30 days ago").
    pub fn before(&self, ms: u64) -> Option<&TrendPoint> {
        let latest = self.latest()?;
        self.points
            .iter()
            .rev()
            .find(|p| p.at_ms + ms <= latest.at_ms)
    }
}

/// Which way maintainability is heading across `points`, ignoring noise under `epsilon`.
pub fn direction(points: &[TrendPoint], epsilon: f64) -> &'static str {
    let scores: Vec<f64> = points.iter().filter_map(|p| p.maintainability).collect();
    let (Some(first), Some(last)) = (scores.first(), scores.last()) else {
        return "unknown";
    };
    let change = last - first;
    if change > epsilon {
        "improving"
    } else if change < -epsilon {
        "declining"
    } else {
        "steady"
    }
}

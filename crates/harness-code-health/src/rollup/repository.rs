use serde::{Deserialize, Serialize};

use super::bands::Band;
use crate::scoring::FileScore;

/// One file as the roll-ups see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileHealth {
    pub path: String,
    pub nloc: u32,
    pub test: bool,
    /// From the Git signals: top-quartile churn with the activity floors.
    pub hotspot: bool,
    /// `None` when the file's language is not measured.
    pub score: Option<FileScore>,
    /// Bug-fix commits in the fix window; `None` when the file has no Git history.
    #[serde(default)]
    pub fix_commits: Option<u32>,
}

impl FileHealth {
    pub(super) fn weight(&self) -> f64 {
        f64::from(self.nloc.max(1))
    }

    pub(super) fn defect(&self) -> Option<f64> {
        self.score.as_ref().map(|s| s.defect.score)
    }

    /// Total defect deduction: the tie-breaker between files the floor makes equal.
    pub(super) fn depth(&self) -> f64 {
        self.score
            .as_ref()
            .map_or(0.0, |s| s.structure_deduction + s.history_deduction)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RollupConfig {
    /// Scored files out of all code files (thousandths) needed before the headline gets a band.
    pub band_min_analysed_per_mille: u32,
}

impl Default for RollupConfig {
    fn default() -> Self {
        Self {
            band_min_analysed_per_mille: 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorstFile {
    pub path: String,
    pub score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BandShare {
    pub band: Band,
    pub files: usize,
    pub nloc: u64,
    /// Share of scored lines of code, in thousandths.
    pub nloc_per_mille: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepositoryHealth {
    /// Line-weighted defect score over scored files.
    pub average: Option<f64>,
    /// Absent when too little of the repository was scored to name a band honestly.
    pub band: Option<Band>,
    /// The same over hotspot files only; `None` when there is no scored hotspot.
    pub hotspot_health: Option<f64>,
    pub maintainability_average: Option<f64>,
    pub maintainability_hotspot: Option<f64>,
    /// Over non-test files.
    pub production_average: Option<f64>,
    /// Line-weighted deduction points from the code itself and from its history.
    pub structure_average: Option<f64>,
    pub history_average: Option<f64>,
    pub structure_hotspot: Option<f64>,
    pub history_hotspot: Option<f64>,
    /// The lowest-scoring non-test file, and the lowest-scoring test.
    pub worst_file: Option<WorstFile>,
    pub worst_test: Option<WorstFile>,
    pub file_count: usize,
    pub production_file_count: usize,
    pub hotspot_count: usize,
    pub unanalysed_count: usize,
    /// Scored files out of all files, in thousandths.
    pub analysed_per_mille: u32,
    /// Scored files and lines per band, worst first.
    pub distribution: Vec<BandShare>,
}

pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Line-weighted mean of `value` over the files that have one.
pub(super) fn weighted<'a>(
    files: impl Iterator<Item = &'a FileHealth>,
    value: impl Fn(&FileHealth) -> Option<f64>,
) -> Option<f64> {
    let (mut sum, mut weight) = (0.0, 0.0);
    for file in files {
        if let Some(v) = value(file) {
            sum += v * file.weight();
            weight += file.weight();
        }
    }
    (weight > 0.0).then(|| round2(sum / weight))
}

/// Worst first: lowest score, then deepest deduction (the floor hides depth), then path. Unscored
/// files sort last.
pub(super) fn worse_first(a: &FileHealth, b: &FileHealth) -> std::cmp::Ordering {
    let (sa, sb) = (
        a.defect().unwrap_or(f64::MAX),
        b.defect().unwrap_or(f64::MAX),
    );
    sa.total_cmp(&sb)
        .then(b.depth().total_cmp(&a.depth()))
        .then(a.path.cmp(&b.path))
}

/// The worst scored file.
pub(super) fn worst<'a>(files: impl Iterator<Item = &'a FileHealth>) -> Option<WorstFile> {
    files
        .filter(|f| f.score.is_some())
        .min_by(|a, b| worse_first(a, b))
        .and_then(|f| {
            f.defect().map(|score| WorstFile {
                path: f.path.clone(),
                score,
            })
        })
}

pub fn repository_health(files: &[FileHealth], config: &RollupConfig) -> RepositoryHealth {
    let scored: Vec<&FileHealth> = files.iter().filter(|f| f.score.is_some()).collect();
    let hotspots = || scored.iter().copied().filter(|f| f.hotspot);
    let production = || scored.iter().copied().filter(|f| !f.test);
    let maintainability = |f: &FileHealth| f.score.as_ref().map(|s| s.maintainability.score);
    let structure = |f: &FileHealth| f.score.as_ref().map(|s| s.structure_deduction);
    let history = |f: &FileHealth| f.score.as_ref().map(|s| s.history_deduction);
    let average = weighted(scored.iter().copied(), FileHealth::defect);
    let analysed_per_mille = if files.is_empty() {
        0
    } else {
        (scored.len() * 1000 / files.len()) as u32
    };
    let total_nloc: u64 = scored.iter().map(|f| u64::from(f.nloc)).sum();
    let distribution = Band::ALL
        .iter()
        .map(|band| {
            let members: Vec<&&FileHealth> = scored
                .iter()
                .filter(|f| f.defect().map(Band::of) == Some(*band))
                .collect();
            let nloc: u64 = members.iter().map(|f| u64::from(f.nloc)).sum();
            BandShare {
                band: *band,
                files: members.len(),
                nloc,
                nloc_per_mille: (nloc * 1000).checked_div(total_nloc).unwrap_or(0) as u32,
            }
        })
        .collect();
    RepositoryHealth {
        band: average
            .filter(|_| analysed_per_mille >= config.band_min_analysed_per_mille)
            .map(Band::of),
        average,
        hotspot_health: weighted(hotspots(), FileHealth::defect),
        maintainability_average: weighted(scored.iter().copied(), maintainability),
        maintainability_hotspot: weighted(hotspots(), maintainability),
        production_average: weighted(production(), FileHealth::defect),
        structure_average: weighted(scored.iter().copied(), structure),
        history_average: weighted(scored.iter().copied(), history),
        structure_hotspot: weighted(hotspots(), structure),
        history_hotspot: weighted(hotspots(), history),
        worst_file: worst(production()),
        worst_test: worst(scored.iter().copied().filter(|f| f.test)),
        file_count: scored.len(),
        production_file_count: production().count(),
        hotspot_count: hotspots().count(),
        unanalysed_count: files.len() - scored.len(),
        analysed_per_mille,
        distribution,
    }
}

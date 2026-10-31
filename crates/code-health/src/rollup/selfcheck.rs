//! Do the lowest scores land on the files that keep getting fixed?
//!
//! Of the `k` lowest-scoring production files with Git history, how many were bug-fixed in the
//! fix window, compared with the share of all such files that were. Bug fixes also feed the score
//! (`prior_defect`), so this is an association on the history already indexed, not a forecast;
//! every result says so. Too few files or too few fixed files give no result at all rather than
//! a number that is mostly noise.
use serde::{Deserialize, Serialize};

use super::repository::{worse_first, FileHealth};

pub const SELF_CHECK_NOTE: &str = "How often the lowest-scoring files were among those bug-fixed in the same history the scores were computed from. An association, not a prediction: recent fixes are one input to the score.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SelfCheckConfig {
    /// How many of the worst files to look at.
    pub k: usize,
    /// Fewer production files with history than this: no result.
    pub min_files: usize,
    /// Fewer bug-fixed files than this: no result.
    pub min_fixed: usize,
}

impl Default for SelfCheckConfig {
    fn default() -> Self {
        Self {
            k: 20,
            min_files: 25,
            min_fixed: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelfCheck {
    /// Files looked at: `k`, or fewer when the repository has fewer.
    pub examined: usize,
    /// Of those, files with at least one bug fix in the window.
    pub fixed_among_worst: usize,
    /// Production files with history, and how many of them were bug-fixed.
    pub files: usize,
    pub fixed_files: usize,
    /// `fixed_among_worst / examined`, in thousandths.
    pub precision_per_mille: u32,
    /// `fixed_files / files`, in thousandths: what picking files at random would give.
    pub base_rate_per_mille: u32,
    /// Precision over base rate, rounded to hundredths; 1.0 means no better than chance.
    pub lift: f64,
    /// `fixed_among_worst / fixed_files`, in thousandths.
    pub recall_per_mille: u32,
    pub window_days: u32,
    /// The worst files looked at, worst first, with whether each was fixed.
    pub worst: Vec<(String, bool)>,
    pub note: String,
}

pub fn self_check(
    files: &[FileHealth],
    window_days: u32,
    config: &SelfCheckConfig,
) -> Option<SelfCheck> {
    let mut population: Vec<&FileHealth> = files
        .iter()
        .filter(|f| !f.test && f.score.is_some() && f.fix_commits.is_some())
        .collect();
    let fixed = |f: &FileHealth| f.fix_commits.is_some_and(|n| n > 0);
    let fixed_files = population.iter().filter(|f| fixed(f)).count();
    if population.len() < config.min_files || fixed_files < config.min_fixed || config.k == 0 {
        return None;
    }
    population.sort_by(|a, b| worse_first(a, b));
    let worst: Vec<(String, bool)> = population
        .iter()
        .take(config.k)
        .map(|f| (f.path.clone(), fixed(f)))
        .collect();
    let examined = worst.len();
    let fixed_among_worst = worst.iter().filter(|(_, fixed)| *fixed).count();
    let precision = fixed_among_worst * 1000 / examined;
    let base_rate = fixed_files * 1000 / population.len();
    Some(SelfCheck {
        examined,
        fixed_among_worst,
        files: population.len(),
        fixed_files,
        precision_per_mille: precision as u32,
        base_rate_per_mille: base_rate as u32,
        lift: ((fixed_among_worst as f64 / examined as f64)
            / (fixed_files as f64 / population.len() as f64)
            * 100.0)
            .round()
            / 100.0,
        recall_per_mille: (fixed_among_worst * 1000 / fixed_files) as u32,
        window_days,
        worst,
        note: SELF_CHECK_NOTE.to_owned(),
    })
}

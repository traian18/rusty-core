//! Five absolute bands over the score, so a 7 means the same in every repository. No letter grade:
//! a third scale on top of the number and the band would only add arbitrary cliffs.
use serde::{Deserialize, Serialize};

pub const EXCELLENT_MIN: f64 = 8.5;
pub const GOOD_MIN: f64 = 7.0;
pub const FAIR_MIN: f64 = 5.5;
pub const NEEDS_WORK_MIN: f64 = 4.0;
/// What a refactoring aims for. Deliberately not a band edge.
pub const TARGET_SCORE: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// Under 4.0.
    AtRisk,
    /// 4.0 to 5.5.
    NeedsWork,
    /// 5.5 to 7.0.
    Fair,
    /// 7.0 to 8.5.
    Good,
    /// 8.5 and above.
    Excellent,
}

impl Band {
    /// Worst first, as lists show them.
    pub const ALL: [Band; 5] = [
        Band::AtRisk,
        Band::NeedsWork,
        Band::Fair,
        Band::Good,
        Band::Excellent,
    ];

    pub fn of(score: f64) -> Self {
        if score >= EXCELLENT_MIN {
            Band::Excellent
        } else if score >= GOOD_MIN {
            Band::Good
        } else if score >= FAIR_MIN {
            Band::Fair
        } else if score >= NEEDS_WORK_MIN {
            Band::NeedsWork
        } else {
            Band::AtRisk
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Band::Excellent => "Excellent",
            Band::Good => "Good",
            Band::Fair => "Fair",
            Band::NeedsWork => "Needs work",
            Band::AtRisk => "At risk",
        }
    }
}

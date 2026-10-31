//! Repository and folder numbers from per-file scores.
//!
//! Every average is weighted by lines of code (`max(nloc, 1)`), so a large file counts for what it
//! holds. Files with no score (a language the metrics cannot read) are left out of every average
//! and counted apart; when none is scored the averages are `None`, never a flattering 10.
mod bands;
mod folders;
mod repository;
mod selfcheck;

pub use bands::{Band, EXCELLENT_MIN, FAIR_MIN, GOOD_MIN, NEEDS_WORK_MIN, TARGET_SCORE};
pub use folders::{folder_rollups, FolderHealth};
pub use repository::{
    repository_health, BandShare, FileHealth, RepositoryHealth, RollupConfig, WorstFile,
};
pub use selfcheck::{self_check, SelfCheck, SelfCheckConfig, SELF_CHECK_NOTE};

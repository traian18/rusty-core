#![warn(clippy::all)]

//! Git intelligence: churn, co-change, and recent-contributor evidence from local history.
//!
//! Git facts are a separate domain from the static import graph. Results describe
//! historical correlation within an analysed window, never causation, dependency, or
//! code ownership.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model`, `config`, `error` | Normalized facts, window and threshold settings, failures |
//! | `contracts` | `GitHistorySource`, `GitAnalysis`, `GitSnapshotRepository` |
//! | `source` | libgit2 history reader |
//! | `history` | Window reading and incremental extension |
//! | `aggregate` | Rename following, churn, co-change, contributors |
//! | `snapshot`, `queries`, `store` | Immutable results, paged queries, retention |
//! | `service` | Builder and the production `GitAnalysis` |
mod aggregate;
mod config;
mod contracts;
mod error;
mod exclusions;
mod history;
mod model;
mod queries;
mod service;
mod snapshot;
mod source;
mod store;

pub use config::GitAnalysisConfig;
pub use contracts::{
    CommitBatch, GitAnalysis, GitHistorySource, GitSnapshotRepository, HeadInfo, HistoryRequest,
    RepositoryCorrelation, WalkStop,
};
pub use error::{GitAnalysisError, GitQueryError};
pub use exclusions::ExclusionSet;
pub use model::{ChangeKind, CommitFact, CommitId, Contributor, ContributorId, FileChange};
pub use queries::{
    ChurnPage, ChurnQuery, ChurnScope, ChurnSort, CoChangePage, CoChangeQuery, ContributorPage,
    ContributorQuery, GitCursor,
};
pub use service::{GitAnalysisBuilder, GitAnalysisService};
pub use snapshot::{
    CoChange, ContributorShare, FileChurn, FileContributors, GitCoverage, GitIndex, GitSnapshot,
    GitSnapshotId, INTERPRETATION,
};
pub use source::Libgit2HistorySource;
pub use store::InMemoryGitSnapshotRepository;

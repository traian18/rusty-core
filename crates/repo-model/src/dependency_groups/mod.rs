//! Dependency groups: declared packages, source roots, inferred directories, and import cycles.
//!
//! Groups are derived from a repository structure and its import index, stored in the
//! snapshot, and queried through [`RepositoryGraph`](crate::repository_intelligence::RepositoryGraph).
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model` | Group, index, and limits data |
//! | `analyzer` | `DependencyGroupAnalyzer`, `GroupRule`, and the default rule-driven analyzer |
//! | `rules` | One rule per group kind |
//! | `scc` | Iterative strongly connected components |
//! | `queries` | List, members, and explanation queries |
//! | `boundary`, `overview`, `projection` | Edge-based queries over groups and files |
mod analyzer;
mod boundary;
mod model;
mod overview;
mod projection;
mod queries;
mod rules;
mod scc;

pub use analyzer::{DefaultGroupAnalyzer, DependencyGroupAnalyzer, GroupInput, GroupRule};
pub use boundary::{BoundaryDirection, BoundaryQuery};
pub use model::{Group, GroupBasis, GroupId, GroupIndex, GroupKind, GroupLimits};
pub use overview::{GroupOverview, OverviewEdge, OverviewQuery};
pub use projection::{
    GraphDirection, GraphEdge, GraphNode, GraphProjection, GraphQuery, Truncation,
};
pub use queries::{
    GroupAssignment, GroupListQuery, GroupMembersQuery, GroupPage, GroupQueries, GroupQueryError,
    GroupSummary, MemberPage,
};
pub use rules::{CycleRule, DeclaredPackageRule, DirectoryRule, SourceRootRule};

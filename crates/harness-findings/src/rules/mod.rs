//! One rule per kind of finding. Rules are pure functions of the snapshots and configuration.
mod boundary;
mod coupling;
mod cycles;
mod hotspot;
mod unattached;
mod undeclared;
mod unreferenced;
mod unused_dependency;

pub use boundary::BoundaryViolationRule;
pub use coupling::ImplicitCouplingRule;
pub use cycles::{ImportCycleRule, TypeOnlyImportCycleRule};
pub use hotspot::ChangeHotspotRule;
pub use unattached::UnattachedFileRule;
pub use undeclared::{UndeclaredDependencyRule, UndeclaredWorkspaceDependencyRule};
pub use unreferenced::UnreferencedFileRule;
pub use unused_dependency::UnusedDeclaredDependencyRule;

use harness_git_analysis::GitSnapshot;
use harness_workspace::repository_intelligence::RepositorySnapshot;

use crate::{
    config::FindingsConfig,
    model::{Draft, FindingKind},
    view::RepoView,
};

/// What a rule needs besides the import index; unmet needs skip the rule and say so.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Requirements {
    pub groups: bool,
    pub git: bool,
}

pub struct FindingsInput<'a> {
    pub repository: &'a RepositorySnapshot,
    pub git: Option<&'a GitSnapshot>,
    pub config: &'a FindingsConfig,
    pub view: &'a RepoView<'a>,
}

pub trait FindingRule: Send + Sync {
    fn id(&self) -> &'static str;
    /// Bump whenever the rule's output could change for the same input.
    fn version(&self) -> u32;
    fn kind(&self) -> FindingKind;
    fn requires(&self) -> Requirements {
        Requirements::default()
    }
    fn description(&self) -> &'static str;
    fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft>;
}

/// Keeps at most `max` items and reports whether anything was dropped.
pub(crate) fn bounded<T>(mut items: Vec<T>, max: usize) -> (Vec<T>, bool) {
    let truncated = items.len() > max;
    items.truncate(max);
    (items, truncated)
}

/// Cargo spells crates with `-` in manifests and `_` in source.
pub(crate) fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

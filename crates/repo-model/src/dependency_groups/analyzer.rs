//! The analyzer contract and the default rule-driven implementation.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    model::{Group, GroupIndex, GroupKind, GroupLimits},
    rules::{CycleRule, DeclaredPackageRule, DirectoryRule, SourceRootRule},
};
use crate::{
    import_graph::{ComponentVersion, ImportIndex},
    repository_intelligence::{DiscoveryError, RepositoryStructure},
};

/// What a rule may look at. Groups are a pure function of these inputs.
pub struct GroupInput<'a> {
    pub structure: &'a RepositoryStructure,
    pub imports: &'a ImportIndex,
    pub limits: &'a GroupLimits,
}

/// Produces the groups of one kind. Rules never see each other's output.
pub trait GroupRule: Send + Sync {
    fn id(&self) -> &'static str;
    /// Bump whenever the rule's output could change for the same input.
    fn version(&self) -> u32;
    fn kind(&self) -> GroupKind;
    fn groups(&self, input: &GroupInput<'_>) -> Vec<Group>;
}

/// Turns a structure and import index into a [`GroupIndex`].
pub trait DependencyGroupAnalyzer: Send + Sync {
    fn analyze(
        &self,
        input: &GroupInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<GroupIndex, DiscoveryError>;

    /// Limits that bound the analysis and the queries over its result.
    fn limits(&self) -> GroupLimits;
}

/// Runs every registered rule in order and merges their groups.
pub struct DefaultGroupAnalyzer {
    rules: Vec<Arc<dyn GroupRule>>,
    limits: GroupLimits,
}

impl DefaultGroupAnalyzer {
    /// Declared packages, source roots, inferred directories, and import cycles.
    pub fn standard() -> Self {
        Self::with_rules(
            vec![
                Arc::new(DeclaredPackageRule),
                Arc::new(SourceRootRule),
                Arc::new(DirectoryRule),
                Arc::new(CycleRule),
            ],
            GroupLimits::default(),
        )
    }

    pub fn with_rules(rules: Vec<Arc<dyn GroupRule>>, limits: GroupLimits) -> Self {
        Self { rules, limits }
    }
}

impl DependencyGroupAnalyzer for DefaultGroupAnalyzer {
    fn analyze(
        &self,
        input: &GroupInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<GroupIndex, DiscoveryError> {
        let mut groups = Vec::new();
        for rule in &self.rules {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            groups.extend(rule.groups(input));
        }
        let rules = self
            .rules
            .iter()
            .map(|rule| ComponentVersion {
                id: format!("group-rule:{}", rule.id()),
                version: rule.version(),
            })
            .collect();
        Ok(GroupIndex::new(groups, rules, self.limits.clone()))
    }

    fn limits(&self) -> GroupLimits {
        self.limits.clone()
    }
}

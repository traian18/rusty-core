//! Builder options for the analyses that run on top of discovery: imports, dependency groups, frameworks.
use std::sync::Arc;

use crate::dependency_groups::DependencyGroupAnalyzer;
use crate::framework_relations::{FrameworkAdapter, FrameworkAnalyzer};
use crate::import_graph::{ImportLimits, ImportSupport};
use crate::repository_intelligence::model::DiscoveryError;

use super::RepositoryGraphBuilder;

impl RepositoryGraphBuilder {
    /// Limits applied to import extraction, resolution, and paging.
    pub fn import_limits(mut self, limits: ImportLimits) -> Self {
        self.import_limits = limits;
        self
    }

    /// Registers a language's adapter and resolver. Two supports may not resolve the same profile.
    pub fn import_support(mut self, support: ImportSupport) -> Result<Self, DiscoveryError> {
        let profiles = support.resolver.profiles();
        let taken = self.import_supports.iter().any(|existing| {
            let existing = existing.resolver.profiles();
            existing.iter().any(|profile| profiles.contains(profile))
        });
        if taken {
            return Err(DiscoveryError::Failed(
                "an import resolver is already registered for one of these profiles".into(),
            ));
        }
        self.import_supports.push(support);
        Ok(self)
    }

    /// Enables dependency groups, cycles, and graph projections over the import index.
    pub fn dependency_groups(mut self, analyzer: Arc<dyn DependencyGroupAnalyzer>) -> Self {
        self.group_analyzer = Some(analyzer);
        self
    }

    /// Registers a framework adapter. Adapters run only for packages where they can show the
    /// framework applies; with none registered no relations are computed.
    pub fn framework_adapter(mut self, adapter: Arc<dyn FrameworkAdapter>) -> Self {
        self.framework_adapters.push(adapter);
        self
    }

    /// Replaces the framework analyzer (the default runs the registered adapters).
    pub fn framework_analyzer(mut self, analyzer: Arc<dyn FrameworkAnalyzer>) -> Self {
        self.framework_analyzer = Some(analyzer);
        self
    }
}

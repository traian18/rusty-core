//! Resolving extracted facts into edges, with each language's resolver.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use tokio_util::sync::CancellationToken;

use super::report::{attribute_edges, facts_for, BuildReport};
use super::ImportPipeline;
use crate::import_graph::model::{
    ConfigFile, ImportBuildError, ImportEdge, LanguageProfile, ResolveContext, SourceUnit,
};
use crate::repository_intelligence::{
    DiscoveryError, PackageId, RepositoryStructure, RootRelativePath,
};

impl ImportPipeline {
    pub(super) fn flag_languages_without_resolver(
        &self,
        units: &[SourceUnit],
        report: &mut BuildReport,
    ) {
        let handled: HashSet<LanguageProfile> = self
            .supports
            .iter()
            .flat_map(|support| support.resolver.profiles())
            .collect();
        for unit in units {
            if !handled.contains(&unit.profile) && !unit.extracted.facts.is_empty() {
                report.omit(&unit.path, "no resolver registered for profile");
            }
        }
    }

    /// Runs each resolver and turns its answers into edges with package attribution.
    pub(super) fn resolve_all(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        units: &[SourceUnit],
        configs: &[ConfigFile],
        cancel: &CancellationToken,
        report: &mut BuildReport,
    ) -> Result<Vec<ImportEdge>, DiscoveryError> {
        let context = ResolveContext {
            root,
            structure,
            configs,
            units,
            limits: &self.limits,
            cancel,
        };
        let packages: HashMap<&RootRelativePath, &Option<PackageId>> =
            units.iter().map(|u| (&u.path, &u.package)).collect();
        let mut edges = Vec::new();
        for support in &self.supports {
            let output = support
                .resolver
                .resolve(&context)
                .map_err(|error| match error {
                    ImportBuildError::Cancelled => DiscoveryError::Cancelled,
                    ImportBuildError::Failed(message) => DiscoveryError::Failed(message),
                })?;
            let facts = facts_for(units, &support.resolver.profiles());
            edges.extend(attribute_edges(
                support.resolver.id(),
                output,
                &facts,
                &packages,
                report,
            )?);
        }
        Ok(edges)
    }
}

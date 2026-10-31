//! The analyzer that runs every adapter on every package and bounds what they find.
use std::{collections::BTreeMap, path::Path};

use tokio_util::sync::CancellationToken;

use super::model::{
    FrameworkApplication, FrameworkIndex, FrameworkIndexParts, FrameworkLimits, FrameworkRelation,
};
use crate::{
    import_graph::ImportIndex,
    repository_intelligence::{
        DiscoveryDiagnostic, DiscoveryError, Package, RepositoryStructure, RootRelativePath,
    },
};

use super::adapter::{
    FrameworkAdapter, FrameworkAnalyzer, FrameworkInput, FrameworkOutput, NameResolver,
};

/// Applies each registered adapter to every package it applies to, enforces the limits, and
/// builds the index. A framework that does not apply to a package contributes nothing to it.
pub struct DefaultFrameworkAnalyzer {
    adapters: Vec<std::sync::Arc<dyn FrameworkAdapter>>,
    limits: FrameworkLimits,
}

impl DefaultFrameworkAnalyzer {
    pub fn new(
        adapters: Vec<std::sync::Arc<dyn FrameworkAdapter>>,
        limits: FrameworkLimits,
    ) -> Self {
        Self { adapters, limits }
    }
}

/// What the adapters have produced so far.
#[derive(Default)]
struct Collected {
    relations: Vec<FrameworkRelation>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    applications: Vec<FrameworkApplication>,
    partial: bool,
}

impl DefaultFrameworkAnalyzer {
    /// Runs one adapter on one package, if it applies, and keeps what it found within the
    /// per-file limit.
    fn run_adapter(
        &self,
        adapter: &dyn FrameworkAdapter,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        collected: &mut Collected,
    ) -> Result<(), DiscoveryError> {
        let Some(evidence) = adapter.applicability(package) else {
            return Ok(());
        };
        collected.applications.push(FrameworkApplication {
            framework: adapter.id().to_owned(),
            package: package.id.clone(),
            evidence,
        });
        let mut output = FrameworkOutput::default();
        adapter.extract(package, input, cancel, &mut output)?;
        collected.diagnostics.extend(output.diagnostics);
        self.keep_within_file_limit(adapter.id(), output.relations, collected);
        Ok(())
    }

    /// Bounds relations per file, keeping the earliest in the file and saying once when the rest
    /// were left out.
    fn keep_within_file_limit(
        &self,
        adapter_id: &str,
        mut relations: Vec<FrameworkRelation>,
        collected: &mut Collected,
    ) {
        let limit = self.limits.max_relations_per_file;
        let mut per_file: BTreeMap<RootRelativePath, usize> = BTreeMap::new();
        relations.sort_by(|a, b| {
            (&a.from, a.evidence.span.start).cmp(&(&b.from, b.evidence.span.start))
        });
        for relation in relations {
            let count = per_file.entry(relation.from.clone()).or_default();
            if *count < limit {
                *count += 1;
                collected.relations.push(relation);
                continue;
            }
            if *count == limit {
                collected.diagnostics.push(DiscoveryDiagnostic::for_path(
                    &relation.from,
                    format!(
                        "{adapter_id} relations in this file exceed the limit of {limit}; the rest were left out"
                    ),
                ));
                *count += 1;
            }
            collected.partial = true;
        }
    }

    /// Cuts the relations to the overall limit, saying so when it does.
    fn keep_within_total_limit(&self, collected: &mut Collected) {
        let limit = self.limits.max_relations;
        if collected.relations.len() <= limit {
            return;
        }
        collected.relations.truncate(limit);
        collected.partial = true;
        collected.diagnostics.push(DiscoveryDiagnostic {
            path: None,
            message: format!(
                "framework relations exceed the limit of {limit}; the rest were left out"
            ),
        });
    }
}

impl FrameworkAnalyzer for DefaultFrameworkAnalyzer {
    fn limits(&self) -> FrameworkLimits {
        self.limits.clone()
    }

    /// Runs every adapter on every package, in package order, and gathers the relations they find.
    fn analyze(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        imports: &ImportIndex,
        cancel: &CancellationToken,
    ) -> Result<FrameworkIndex, DiscoveryError> {
        let names = NameResolver::from_imports(imports);
        let input = FrameworkInput {
            root,
            structure,
            imports,
            names: &names,
            limits: &self.limits,
        };
        let mut packages = structure.packages.iter().collect::<Vec<_>>();
        packages.sort_by(|a, b| a.id.cmp(&b.id));

        let mut collected = Collected::default();
        for package in packages {
            for adapter in &self.adapters {
                if cancel.is_cancelled() {
                    return Err(DiscoveryError::Cancelled);
                }
                self.run_adapter(adapter.as_ref(), package, &input, cancel, &mut collected)?;
            }
        }
        self.keep_within_total_limit(&mut collected);
        Ok(FrameworkIndex::new(FrameworkIndexParts {
            relations: collected.relations,
            diagnostics: collected.diagnostics,
            applications: collected.applications,
            capabilities: self.adapters.iter().map(|a| a.capability()).collect(),
            partial: collected.partial,
            limits: self.limits.clone(),
        }))
    }
}

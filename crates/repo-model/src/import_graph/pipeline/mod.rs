//! Extraction, caching, and resolution orchestration for one refresh.
//!
//! A refresh runs four steps: extract facts from every source file (cached by content), load
//! compiler configs, resolve facts with each language's resolver, and assemble the immutable
//! [`ImportIndex`]. `extract`, `resolve`, `cache`, `ecosystem` and `report` hold the steps.

mod cache;
mod ecosystem;
mod extract;
mod report;
mod resolve;

pub use cache::{CacheStats, ExtractionCache};

use std::{path::Path, sync::Arc, time::Instant};

use tokio_util::sync::CancellationToken;

use crate::import_graph::{
    index::{ComponentVersion, ImportIndex, ImportIndexParts},
    metrics::{ImportMetrics, ImportPhaseDurations},
    model::{ImportLimits, ImportResolver, LanguageImportAdapter},
};
use crate::repository_intelligence::{DiscoveryError, RepositoryStructure};
use report::BuildReport;

/// One language's extraction and resolution, registered together.
#[derive(Clone)]
pub struct ImportSupport {
    pub adapter: Arc<dyn LanguageImportAdapter>,
    pub resolver: Arc<dyn ImportResolver>,
}

/// A finished index and what it took to build.
pub struct ImportBuild {
    pub index: ImportIndex,
    pub metrics: ImportMetrics,
}

/// Builds import indexes for a set of registered languages.
pub struct ImportPipeline {
    pub(super) supports: Vec<ImportSupport>,
    pub(super) limits: ImportLimits,
    pub(super) cache: ExtractionCache,
}

impl ImportPipeline {
    pub fn new(supports: Vec<ImportSupport>, limits: ImportLimits) -> Self {
        Self {
            supports,
            limits,
            cache: ExtractionCache::new(100_000),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.supports.is_empty()
    }

    pub fn supports(&self) -> &[ImportSupport] {
        &self.supports
    }

    pub fn limits(&self) -> &ImportLimits {
        &self.limits
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    pub fn build(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        cancel: &CancellationToken,
    ) -> Result<ImportBuild, DiscoveryError> {
        let mut report = BuildReport::default();
        let cache_before = self.cache.stats();
        let started = Instant::now();
        let extraction = self.extract_sources(root, structure, cancel, &mut report)?;
        let extracted = started.elapsed();
        let (configs, indexed_configs) = self.load_configs(root, structure, cancel, &mut report)?;
        let configured = started.elapsed();
        self.flag_languages_without_resolver(&extraction.units, &mut report);
        let edges = self.resolve_all(
            root,
            structure,
            &extraction.units,
            &configs,
            cancel,
            &mut report,
        )?;
        let resolved = started.elapsed();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let index = ImportIndex::new(ImportIndexParts {
            files: extraction.files,
            configs: indexed_configs,
            unattached: report.unattached,
            edges,
            diagnostics: report.diagnostics,
            partial: report.partial,
            limits: self.limits.clone(),
            components: self.component_versions(),
        });
        let durations = ImportPhaseDurations {
            extraction: extracted,
            config_loading: configured - extracted,
            resolution: resolved - configured,
            assembly: started.elapsed() - resolved,
        };
        let cache = self.cache.stats();
        let cache_delta = CacheStats {
            hits: cache.hits - cache_before.hits,
            misses: cache.misses - cache_before.misses,
        };
        let metrics = ImportMetrics::from_index(&index, extraction.bytes, cache_delta, durations);
        Ok(ImportBuild { index, metrics })
    }

    fn component_versions(&self) -> Vec<ComponentVersion> {
        self.supports
            .iter()
            .flat_map(|support| {
                [
                    ComponentVersion {
                        id: format!("adapter:{}", support.adapter.id()),
                        version: support.adapter.version(),
                    },
                    ComponentVersion {
                        id: format!("resolver:{}", support.resolver.id()),
                        version: support.resolver.version(),
                    },
                ]
            })
            .collect()
    }
}

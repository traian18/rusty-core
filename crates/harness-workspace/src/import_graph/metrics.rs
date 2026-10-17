//! Aggregate measurements of one import-index build. Counts and durations only: no paths, no source text.
use std::time::Duration;

use super::{index::ImportIndex, model::ResolutionKind, pipeline::CacheStats};

/// Time spent in each phase of one build.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportPhaseDurations {
    pub extraction: Duration,
    pub config_loading: Duration,
    pub resolution: Duration,
    pub assembly: Duration,
}

/// What one build processed and produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportMetrics {
    pub files: usize,
    pub source_bytes: u64,
    pub configs: usize,
    pub facts: usize,
    pub resolved_internal: usize,
    pub declared_external: usize,
    pub possible: usize,
    pub unresolved: usize,
    pub dynamic_or_unsupported: usize,
    pub diagnostics: usize,
    pub partial: bool,
    /// Extraction-cache hits and misses caused by this build alone.
    pub cache: CacheStats,
    pub durations: ImportPhaseDurations,
}

impl ImportMetrics {
    /// Counts taken from the finished index.
    pub(super) fn from_index(
        index: &ImportIndex,
        source_bytes: u64,
        cache: CacheStats,
        durations: ImportPhaseDurations,
    ) -> Self {
        let mut metrics = Self {
            files: index.files.len(),
            source_bytes,
            configs: index.configs.len(),
            facts: index.edges.len(),
            diagnostics: index.diagnostics.len(),
            partial: index.partial,
            cache,
            durations,
            ..Self::default()
        };
        for edge in &index.edges {
            let counter = match edge.resolution.kind() {
                ResolutionKind::ResolvedInternal => &mut metrics.resolved_internal,
                ResolutionKind::DeclaredExternal => &mut metrics.declared_external,
                ResolutionKind::Possible => &mut metrics.possible,
                ResolutionKind::Unresolved => &mut metrics.unresolved,
                ResolutionKind::DynamicOrUnsupported => &mut metrics.dynamic_or_unsupported,
            };
            *counter += 1;
        }
        metrics
    }
}

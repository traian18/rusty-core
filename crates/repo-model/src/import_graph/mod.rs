//! File and module import dependency graph (roadmap Phase 2).
//!
//! The contracts here are language-neutral; concrete adapters and resolvers live
//! outside this crate and are registered through `RepositoryGraphBuilder`.
mod helpers;
mod index;
mod metrics;
mod model;
mod pipeline;

pub use helpers::{resolve_units, Decision, FactBuilder};
pub use index::{
    ComponentVersion, Direction, ImportCursor, ImportFilter, ImportIndex, ImportIndexParts,
    ImportPage, ImportQuery, ImportQueryError, IndexedConfig, IndexedFile,
};
pub use metrics::{ImportMetrics, ImportPhaseDurations};
pub use model::{
    ByteSpan, ConfigFile, DeclaredItem, ExtractedFile, ImportBuildError, ImportCapabilities,
    ImportEdge, ImportFact, ImportFactId, ImportForm, ImportKind, ImportLimits, ImportReason,
    ImportResolver, ItemKind, ItemStatus, LanguageCapability, LanguageImportAdapter,
    LanguageProfile, PathAttribute, ReexportHop, Resolution, ResolutionKind, ResolveContext,
    ResolveOutput, ResolvedImport, SourceUnit, Staticness, Visibility,
};
pub use pipeline::{CacheStats, ExtractionCache, ImportBuild, ImportPipeline, ImportSupport};

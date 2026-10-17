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
pub use index::*;
pub use metrics::{ImportMetrics, ImportPhaseDurations};
pub use model::*;
pub use pipeline::{CacheStats, ExtractionCache, ImportBuild, ImportPipeline, ImportSupport};

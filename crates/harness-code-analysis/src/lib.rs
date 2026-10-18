mod adapters;
mod api;
mod builder;
pub mod metrics;
mod model;
mod query;
mod service;
mod snapshot_repository;
mod source;
#[cfg(unix)]
mod source_unix;

pub use adapters::{
    CallFact, CallKind, CallableFact, ExportFact, ExtractedFile, ImportFact, ImportKind,
    LanguageAdapter, LanguageResolver, LocalBinding, Outcome, ProjectIndex, Receiver,
    ResolutionContext, TargetRef, TypeFact, TypeKind,
};
pub use api::{CallGraph, CallGraphError};
pub use builder::CallGraphBuilder;
pub use model::*;
pub use service::DefaultCallGraph;
pub use snapshot_repository::{GraphSnapshot, InMemorySnapshotRepository, SnapshotRepository};
pub use source::{AnalysisSource, FileSystemSource, OpenHook, SourceDiscovery, SourceFile};

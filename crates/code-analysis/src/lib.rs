#![warn(clippy::undocumented_unsafe_blocks)]

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
pub use model::{
    CallGraphCapabilities, CallSite, CallableKind, CallableResponse, CallableSymbol,
    CallsFromRequest, CallsResponse, CallsToRequest, Diagnostic, Direction, Evidence,
    FindCallablesRequest, LanguageCapabilities, LanguageId, Page, QueryMetadata, RefreshRequest,
    RefreshResponse, RefreshStats, Resolution, SnapshotId, SourceSpan, SymbolId, TraversalLimit,
    TraversalRequest, TraversalResponse,
};
pub use service::DefaultCallGraph;
pub use snapshot_repository::{GraphSnapshot, InMemorySnapshotRepository, SnapshotRepository};
pub use source::{AnalysisSource, FileSystemSource, OpenHook, SourceDiscovery, SourceFile};

use crate::adapters::{AdapterRegistry, LanguageAdapter, LanguageResolver};
use crate::service::DefaultCallGraph;
use crate::snapshot_repository::{InMemorySnapshotRepository, SnapshotRepository};
use crate::source::{AnalysisSource, FileSystemSource};
use crate::CallGraphError;
use std::path::PathBuf;
use std::sync::Arc;

/// Validated composition for a call-graph service scoped to one authorized root.
pub struct CallGraphBuilder {
    root: PathBuf,
    source: Option<Arc<dyn AnalysisSource>>,
    snapshots: Option<Arc<dyn SnapshotRepository>>,
    adapters: Vec<Arc<dyn LanguageAdapter>>,
    resolver: Option<Arc<dyn LanguageResolver>>,
}

impl CallGraphBuilder {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            source: None,
            snapshots: None,
            adapters: Vec::new(),
            resolver: None,
        }
    }

    pub fn source(mut self, source: Arc<dyn AnalysisSource>) -> Self {
        self.source = Some(source);
        self
    }

    pub fn snapshot_repository(mut self, repository: Arc<dyn SnapshotRepository>) -> Self {
        self.snapshots = Some(repository);
        self
    }

    pub fn adapter(mut self, adapter: Arc<dyn LanguageAdapter>) -> Self {
        self.adapters.push(adapter);
        self
    }

    pub fn resolver(mut self, resolver: Arc<dyn LanguageResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    pub fn build(self) -> Result<DefaultCallGraph, CallGraphError> {
        if !self.root.is_absolute() {
            return Err(CallGraphError::InvalidRequest(
                "root must be absolute".into(),
            ));
        }
        let custom_adapters = !self.adapters.is_empty();
        if !custom_adapters && self.resolver.is_some() {
            return Err(CallGraphError::InvalidRequest(
                "a custom resolver requires custom adapters".into(),
            ));
        }
        let adapters = if custom_adapters {
            AdapterRegistry::new(
                self.adapters,
                self.resolver
                    .unwrap_or_else(|| Arc::new(crate::adapters::StaticResolver)),
            )?
        } else {
            AdapterRegistry::builtin()
        };
        let source = self
            .source
            .unwrap_or_else(|| Arc::new(FileSystemSource::new(8 * 1024 * 1024, 32)));
        let snapshots = match self.snapshots {
            Some(repository) => repository,
            None => Arc::new(InMemorySnapshotRepository::new(4)?),
        };
        Ok(DefaultCallGraph::with_dependencies(
            self.root, source, snapshots, adapters,
        ))
    }
}

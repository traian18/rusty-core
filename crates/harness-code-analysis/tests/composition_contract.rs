use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_code_analysis::{
    AnalysisSource, CallGraph, CallGraphBuilder, CallGraphError, LanguageAdapter,
    LanguageCapabilities, LanguageId, LanguageResolver, SourceDiscovery,
};
use tokio_util::sync::CancellationToken;

struct EmptySource;

#[async_trait]
impl AnalysisSource for EmptySource {
    async fn discover(
        &self,
        _root: &Path,
        _max_files: usize,
        _cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError> {
        Ok(SourceDiscovery::default())
    }
}

struct MinimalAdapter;

impl LanguageAdapter for MinimalAdapter {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }
    fn version(&self) -> &'static str {
        "test-1"
    }
    fn capabilities(&self) -> LanguageCapabilities {
        LanguageCapabilities {
            language: LanguageId::Rust,
            supported: true,
            limitations: Vec::new(),
            adapter_version: String::new(),
            resolver_version: String::new(),
        }
    }
    fn extract(
        &self,
        _path: &str,
        _source: &str,
    ) -> Result<harness_code_analysis::ExtractedFile, CallGraphError> {
        Ok(harness_code_analysis::ExtractedFile::empty(
            LanguageId::Rust,
        ))
    }
}

struct EmptyResolver;

impl LanguageResolver for EmptyResolver {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }
    fn version(&self) -> &'static str {
        "empty-1"
    }
    fn resolve(
        &self,
        _ctx: &harness_code_analysis::ResolutionContext<'_>,
        _call: &harness_code_analysis::CallFact,
    ) -> harness_code_analysis::Outcome {
        harness_code_analysis::Outcome::Unresolved {
            rule: "empty",
            explanation: "substitute resolver".into(),
        }
    }
}

#[tokio::test]
async fn builder_composes_substitute_source_adapter_and_resolver() {
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(Arc::new(EmptySource))
        .adapter(Arc::new(MinimalAdapter))
        .resolver(Arc::new(EmptyResolver))
        .build()
        .unwrap();

    let response = graph
        .refresh(
            harness_code_analysis::RefreshRequest { max_files: None },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.metadata.snapshot, response.snapshot);
    assert_eq!(graph.capabilities().languages.len(), 1);
}

#[test]
fn builder_rejects_invalid_composition() {
    assert!(matches!(
        CallGraphBuilder::new("relative-root").build(),
        Err(CallGraphError::InvalidRequest(_))
    ));
    assert!(matches!(
        CallGraphBuilder::new(std::env::temp_dir())
            .resolver(Arc::new(EmptyResolver))
            .build(),
        Err(CallGraphError::InvalidRequest(_))
    ));
}

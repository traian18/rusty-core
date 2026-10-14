use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_code_analysis::{
    AnalysisSource, CallGraph, CallGraphBuilder, CallGraphError, LanguageAdapter,
    LanguageCapabilities, LanguageId, LanguageResolver, SourceFile,
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
    ) -> Result<Vec<SourceFile>, CallGraphError> {
        Ok(Vec::new())
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
        }
    }
    fn extract(
        &self,
        _path: &str,
        _source: &str,
    ) -> Result<harness_code_analysis::ExtractedFile, CallGraphError> {
        Ok(harness_code_analysis::ExtractedFile {
            language: LanguageId::Rust,
            callables: Vec::new(),
            calls: Vec::new(),
            imports: Vec::new(),
            diagnostics: Vec::new(),
        })
    }
}

struct EmptyResolver;

impl LanguageResolver for EmptyResolver {
    fn imported_name<'a>(
        &self,
        _file: &'a harness_code_analysis::ExtractedFile,
        _local: &str,
    ) -> Option<&'a harness_code_analysis::ImportFact> {
        None
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

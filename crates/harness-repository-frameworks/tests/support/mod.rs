#![allow(dead_code)]
use std::{path::Path, sync::Arc};

use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    framework_relations::{FrameworkAdapter, FrameworkRelation},
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder, RepositorySnapshot},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub struct Repo {
    pub dir: TempDir,
    pub snapshot: Arc<RepositorySnapshot>,
}

pub fn write(root: &Path, relative: &str, text: &str) {
    let full = root.join(relative);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

/// Writes `files`, builds a snapshot with Rust and TypeScript/JavaScript imports and `adapters`.
pub async fn repo(files: &[(&str, &str)], adapters: Vec<Arc<dyn FrameworkAdapter>>) -> Repo {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    let mut manifests = harness_workspace::repository_intelligence::default_manifest_adapters();
    manifests.extend(harness_repository_imports_python::manifest_adapters());
    manifests.extend(harness_repository_imports_go::manifest_adapters());
    manifests.extend(harness_repository_imports_java::manifest_adapters());
    manifests.extend(harness_repository_imports_csharp::manifest_adapters());
    manifests.extend(harness_repository_imports_php::manifest_adapters());
    let mut extensions: Vec<String> = ["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"]
        .iter()
        .map(|e| e.to_string())
        .collect();
    extensions.extend(
        harness_repository_imports_python::SOURCE_EXTENSIONS
            .iter()
            .map(|e| e.to_string()),
    );
    extensions.extend(
        harness_repository_imports_go::SOURCE_EXTENSIONS
            .iter()
            .map(|e| e.to_string()),
    );
    extensions.extend(
        harness_repository_imports_java::SOURCE_EXTENSIONS
            .iter()
            .map(|e| e.to_string()),
    );
    extensions.extend(
        harness_repository_imports_csharp::SOURCE_EXTENSIONS
            .iter()
            .map(|e| e.to_string()),
    );
    extensions.extend(
        harness_repository_imports_php::SOURCE_EXTENSIONS
            .iter()
            .map(|e| e.to_string()),
    );
    let mut builder = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .manifest_adapters(manifests)
        .source_extensions(extensions)
        .import_support(harness_repository_imports_python::import_support())
        .unwrap()
        .import_support(harness_repository_imports_go::import_support())
        .unwrap()
        .import_support(harness_repository_imports_java::import_support())
        .unwrap()
        .import_support(harness_repository_imports_csharp::import_support())
        .unwrap()
        .import_support(harness_repository_imports_php::import_support())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(TypeScriptImportAdapter),
            resolver: Arc::new(TypeScriptImportResolver),
        })
        .unwrap();
    for adapter in adapters {
        builder = builder.framework_adapter(adapter);
    }
    let service = builder.build();
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    Repo {
        dir,
        snapshot: Arc::new(snapshot),
    }
}

impl Repo {
    pub fn relations(&self) -> &[FrameworkRelation] {
        &self.snapshot.frameworks.relations
    }

    /// `kind|label|from->to` for each relation, in snapshot order, to compare at a glance.
    pub fn summary(&self) -> Vec<String> {
        self.relations()
            .iter()
            .map(|r| {
                format!(
                    "{:?}|{}|{}->{}",
                    r.kind,
                    r.label,
                    r.from.as_path().display(),
                    r.to.as_ref()
                        .map(|t| t.as_path().display().to_string())
                        .unwrap_or_else(|| "-".into())
                )
            })
            .collect()
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.snapshot
            .frameworks
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }
}

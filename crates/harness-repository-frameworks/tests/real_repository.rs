//! Runs every adapter over a real checkout and prints what it found. Ignored by default:
//! `FRAMEWORK_SCAN_ROOT=/path/to/repo cargo test -p harness-repository-frameworks --test real_repository -- --ignored --nocapture`
use std::{collections::BTreeMap, sync::Arc, time::Instant};

use harness_repository_frameworks::*;
use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "scans a real repository; run by hand"]
async fn scan_a_real_repository() {
    let root = std::env::var("FRAMEWORK_SCAN_ROOT").expect("set FRAMEWORK_SCAN_ROOT");
    let service = RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
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
        .unwrap()
        .framework_adapter(Arc::new(ExpressAdapter))
        .framework_adapter(Arc::new(NextjsAdapter))
        .framework_adapter(Arc::new(NestjsAdapter))
        .framework_adapter(Arc::new(ReactAdapter))
        .framework_adapter(Arc::new(AngularAdapter))
        .framework_adapter(Arc::new(VueAdapter))
        .framework_adapter(Arc::new(AxumAdapter))
        .framework_adapter(Arc::new(ActixAdapter))
        .framework_adapter(Arc::new(TauriAdapter))
        .build();
    let started = Instant::now();
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    println!("refresh: {:?}", started.elapsed());
    if let Some(metrics) = service.last_refresh_metrics() {
        println!("frameworks stage: {:?}", metrics.frameworks);
    }
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for relation in &snapshot.frameworks.relations {
        *counts
            .entry((relation.framework.clone(), format!("{:?}", relation.kind)))
            .or_default() += 1;
    }
    for ((framework, kind), count) in counts {
        println!("{framework:8} {kind:22} {count}");
    }
    for application in &snapshot.frameworks.applications {
        println!(
            "applies: {} in {}",
            application.framework, application.package.0
        );
    }
    println!("diagnostics: {}", snapshot.frameworks.diagnostics.len());
    for diagnostic in snapshot.frameworks.diagnostics.iter().take(8) {
        println!("  {:?}: {}", diagnostic.path, diagnostic.message);
    }
    for relation in snapshot.frameworks.relations.iter().take(6) {
        println!(
            "  e.g. {:?} {} {} -> {:?}",
            relation.kind,
            relation.label,
            relation.from.as_path().display(),
            relation
                .to
                .as_ref()
                .map(|t| t.as_path().display().to_string())
        );
    }
}

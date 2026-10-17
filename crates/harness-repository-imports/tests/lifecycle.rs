mod support;

use std::{collections::BTreeSet, sync::Arc};

use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    import_graph::{
        ExtractedFile, ImportForm, ImportLimits, ImportReason, ImportResolver, ImportSupport,
        LanguageImportAdapter, LanguageProfile, Resolution,
    },
    repository_intelligence::{
        DiscoveryError, RepositoryGraph, RepositoryGraphBuilder, RootRelativePath,
    },
};
use support::*;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn single_crate(dir: &std::path::Path) {
    write(
        dir,
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir,
        "src/lib.rs",
        "pub mod a;\npub mod b;\nuse crate::a::One;\n",
    );
    write(dir, "src/a.rs", "pub struct One;\n");
    write(dir, "src/b.rs", "use crate::a::One;\n");
}

#[tokio::test]
async fn refresh_is_deterministic_and_reuses_unchanged_extractions() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    let graph = service(dir.path());
    let first = refresh(&graph).await;
    let misses = graph.import_cache_stats().misses;
    assert_eq!(misses, 3);
    let second = refresh(&graph).await;
    assert_eq!(first, second);
    let stats = graph.import_cache_stats();
    assert_eq!((stats.hits, stats.misses), (3, 3));

    // A fresh service over the same tree yields the same identity and index.
    let other = service(dir.path());
    let third = refresh(&other).await;
    assert_eq!(first, third);
    assert_eq!(
        graph.current().unwrap().imports,
        other.current().unwrap().imports
    );

    // One-file edit: one extraction miss, new snapshot identity, resolution rerun.
    write(
        dir.path(),
        "src/b.rs",
        "use crate::a::One;\nuse crate::a::Two;\n",
    );
    let edited = refresh(&graph).await;
    assert_ne!(first, edited);
    let stats = graph.import_cache_stats();
    assert_eq!((stats.hits, stats.misses), (5, 4));
    match &edge(&outgoing(&graph, &edited, "src/b.rs"), "crate::a::Two").resolution {
        Resolution::ResolvedInternal { item_status, .. } => {
            assert_eq!(
                *item_status,
                harness_workspace::import_graph::ItemStatus::NotFound
            )
        }
        other => panic!("{other:?}"),
    }
    // Immutable older generation still answers with its own data.
    assert_eq!(outgoing(&graph, &first, "src/b.rs").len(), 1);
}

#[tokio::test]
async fn create_delete_rename_and_manifest_changes_are_reflected() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    write(dir.path(), "src/c.rs", "pub fn c() {}\n");
    write(
        dir.path(),
        "src/lib.rs",
        "pub mod a;\npub mod b;\npub mod c;\nuse serde::Serialize;\n",
    );
    let graph = service(dir.path());
    let base = refresh(&graph).await;
    assert_eq!(incoming(&graph, &base, "src/c.rs").len(), 1);
    assert!(matches!(
        edge(&outgoing(&graph, &base, "src/lib.rs"), "serde::Serialize").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));

    // Manifest change: declaring the dependency changes the resolution.
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n",
    );
    let declared = refresh(&graph).await;
    assert_ne!(base, declared);
    assert!(matches!(
        edge(
            &outgoing(&graph, &declared, "src/lib.rs"),
            "serde::Serialize"
        )
        .resolution,
        Resolution::DeclaredExternal { .. }
    ));

    // Rename: the declaration no longer resolves; the file is unattached.
    std::fs::rename(dir.path().join("src/c.rs"), dir.path().join("src/d.rs")).unwrap();
    let renamed = refresh(&graph).await;
    assert!(matches!(
        edge(&outgoing(&graph, &renamed, "src/lib.rs"), "c").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    assert!(graph
        .current()
        .unwrap()
        .imports
        .diagnostics
        .iter()
        .any(|d| d.message.contains("unattached")));

    // Delete: the importer's file disappears from the index.
    std::fs::remove_file(dir.path().join("src/b.rs")).unwrap();
    let deleted = refresh(&graph).await;
    assert!(graph
        .dependencies(&query(&deleted, "src/b.rs", 10))
        .is_err());
}

struct Versioned(u32);
impl LanguageImportAdapter for Versioned {
    fn id(&self) -> &'static str {
        RustImportAdapter.id()
    }
    fn version(&self) -> u32 {
        self.0
    }
    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        RustImportAdapter.profile_for(path)
    }
    fn forms(&self) -> Vec<ImportForm> {
        RustImportAdapter.forms()
    }
    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        RustImportAdapter.extract(path, profile, source, limits)
    }
}

fn with_adapter(
    root: &std::path::Path,
    adapter: Arc<dyn LanguageImportAdapter>,
    limits: ImportLimits,
) -> harness_workspace::repository_intelligence::RepositoryGraphService {
    RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
        .unwrap()
        .import_limits(limits)
        .import_support(ImportSupport {
            adapter,
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .build()
}

#[tokio::test]
async fn adapter_version_and_limits_change_identity() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    let v1 = refresh(&with_adapter(
        dir.path(),
        Arc::new(Versioned(1)),
        ImportLimits::default(),
    ))
    .await;
    let v2 = refresh(&with_adapter(
        dir.path(),
        Arc::new(Versioned(2)),
        ImportLimits::default(),
    ))
    .await;
    assert_ne!(v1, v2);
    let limited = ImportLimits {
        max_candidates: 3,
        ..ImportLimits::default()
    };
    let l = refresh(&with_adapter(dir.path(), Arc::new(Versioned(1)), limited)).await;
    assert_ne!(v1, l);
}

#[tokio::test]
async fn limits_produce_partial_coverage_with_diagnostics() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    write(
        dir.path(),
        "src/big.rs",
        &format!("{}\n", "// padding ".repeat(2000)),
    );
    let small = ImportLimits {
        max_file_bytes: 1024,
        ..ImportLimits::default()
    };
    let graph = with_adapter(dir.path(), Arc::new(Versioned(1)), small);
    let id = refresh(&graph).await;
    let index = graph.current().unwrap();
    assert!(index.imports.partial);
    assert!(index
        .imports
        .diagnostics
        .iter()
        .any(|d| d.message.contains("byte limit")));
    assert!(!index.imports.has_file(&path("src/big.rs")));
    assert!(
        graph
            .dependencies(&query(&id, "src/lib.rs", 10))
            .unwrap()
            .partial_coverage
    );

    let few = ImportLimits {
        max_facts_per_file: 1,
        ..ImportLimits::default()
    };
    let graph = with_adapter(dir.path(), Arc::new(Versioned(1)), few);
    refresh(&graph).await;
    let snapshot = graph.current().unwrap();
    assert!(snapshot.imports.partial);
    assert!(snapshot
        .imports
        .diagnostics
        .iter()
        .any(|d| d.message.contains("per-file import fact limit")));

    let total = ImportLimits {
        max_total_facts: 2,
        ..ImportLimits::default()
    };
    let graph = with_adapter(dir.path(), Arc::new(Versioned(1)), total);
    refresh(&graph).await;
    let snapshot = graph.current().unwrap();
    assert!(snapshot.imports.partial);
    assert!(snapshot.imports.edges.len() <= 2);
}

#[tokio::test]
async fn cancelled_refresh_keeps_the_prior_snapshot() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    let graph = service(dir.path());
    let first = refresh(&graph).await;
    write(dir.path(), "src/b.rs", "pub fn changed() {}\n");
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        graph.refresh(cancel).await,
        Err(DiscoveryError::Cancelled)
    ));
    assert_eq!(graph.current().unwrap().id, first);
    assert_eq!(graph.import_cache_stats().misses, 3);
}

struct Incomplete;
impl ImportResolver for Incomplete {
    fn id(&self) -> &'static str {
        "incomplete"
    }
    fn version(&self) -> u32 {
        1
    }
    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::Rust]
    }
    fn rules(&self) -> Vec<&'static str> {
        vec![]
    }
    fn unsupported(&self) -> Vec<&'static str> {
        vec![]
    }
    fn resolve(
        &self,
        _: &harness_workspace::import_graph::ResolveContext<'_>,
    ) -> Result<
        harness_workspace::import_graph::ResolveOutput,
        harness_workspace::import_graph::ImportBuildError,
    > {
        Ok(Default::default())
    }
}

#[tokio::test]
async fn resolver_failures_do_not_publish_and_duplicate_profiles_are_rejected() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    let broken = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(Incomplete),
        })
        .unwrap()
        .build();
    assert!(broken.refresh(CancellationToken::new()).await.is_err());
    assert!(broken.current().is_none());

    let duplicate = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        });
    assert!(duplicate.is_err());
}

#[tokio::test]
async fn advertised_rules_and_forms_are_demonstrated_by_fixtures() {
    let dir = tempdir().unwrap();
    rust_workspace(dir.path());
    // Distinct subtree so both ecosystems live in one repository.
    ts_workspace(&dir.path().join("web"));
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let capabilities = graph.import_capabilities();
    let snapshot = graph.current().unwrap();
    assert_eq!(snapshot.id, id);
    let seen_rules = snapshot
        .imports
        .edges
        .iter()
        .map(|e| e.rule.as_str())
        .collect::<BTreeSet<_>>();
    let seen_forms = snapshot
        .imports
        .edges
        .iter()
        .map(|e| e.fact.form)
        .collect::<BTreeSet<_>>();
    assert_eq!(capabilities.languages.len(), 2);
    for language in &capabilities.languages {
        assert!(!language.unsupported.is_empty());
        for rule in &language.resolution_rules {
            assert!(
                seen_rules.contains(rule.as_str()),
                "rule {rule} is advertised but never exercised"
            );
        }
        for form in &language.forms {
            assert!(
                seen_forms.contains(form),
                "form {form:?} is advertised but never exercised"
            );
        }
    }
    // Every edge cites a rule the resolvers advertise.
    let advertised = capabilities
        .languages
        .iter()
        .flat_map(|l| l.resolution_rules.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    assert!(
        seen_rules.is_subset(&advertised),
        "{:?}",
        seen_rules.difference(&advertised).collect::<Vec<_>>()
    );
    let _ = (TypeScriptImportAdapter, TypeScriptImportResolver);
}

#[tokio::test]
async fn refresh_reports_phase_timings_counts_and_cache_activity() {
    let dir = tempdir().unwrap();
    single_crate(dir.path());
    let graph = service(dir.path());
    assert!(graph.last_refresh_metrics().is_none());
    refresh(&graph).await;
    let cold = graph.last_refresh_metrics().unwrap();
    let details = cold.import_details.clone().unwrap();
    assert_eq!(details.files, 3);
    assert_eq!(details.facts, 4);
    assert_eq!(details.resolved_internal, 4);
    assert_eq!((details.cache.hits, details.cache.misses), (0, 3));
    assert!(details.source_bytes > 0 && !details.partial);
    assert!(cold.total >= cold.discovery);

    refresh(&graph).await;
    let warm = graph
        .last_refresh_metrics()
        .unwrap()
        .import_details
        .unwrap();
    assert_eq!((warm.cache.hits, warm.cache.misses), (3, 0));

    let without_support = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .build();
    without_support
        .refresh(CancellationToken::new())
        .await
        .unwrap();
    assert!(without_support
        .last_refresh_metrics()
        .unwrap()
        .import_details
        .is_none());
}

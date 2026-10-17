//! The framework contract, exercised with a fake adapter that needs no parser.
use std::sync::Arc;

use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    import_graph::{ByteSpan, ComponentVersion},
    repository_intelligence::{
        DiscoveryError, Package, RepositoryGraph, RepositoryGraphBuilder, RootRelativePath,
    },
};

/// Emits one route relation per source file of packages that declare `fakefw`.
struct FakeAdapter {
    version: u32,
    per_file: usize,
}

impl FrameworkAdapter for FakeAdapter {
    fn id(&self) -> &'static str {
        "fakefw"
    }
    fn version(&self) -> u32 {
        self.version
    }
    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: "fakefw".into(),
            name: "Fake".into(),
            adapter_version: self.version,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec!["every source file".into()],
            unsupported: vec!["nothing else".into()],
        }
    }
    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "fakefw")
    }
    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let dir = FrameworkInput::package_dir(package);
        for file in &input.structure.source_files {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            if !file.as_path().starts_with(&dir) {
                continue;
            }
            let text = input.read(file).unwrap_or_default();
            if text.contains("dynamic") {
                output.unresolved(file, "route path is computed; no relation emitted");
                continue;
            }
            for n in 0..self.per_file {
                output.relate(FrameworkRelation::new(
                    "fakefw",
                    Some(&package.id),
                    RelationDraft {
                        kind: RelationKind::RouteRegisteredTo,
                        rule: "fake-route",
                        rule_version: 1,
                        certainty: Certainty::Declared,
                        from: file.clone(),
                        to: Some(file.clone()),
                        from_symbol: None,
                        to_symbol: Some("handler".into()),
                        label: format!("GET /{n}"),
                        evidence: RelationEvidence::new(
                            file,
                            ByteSpan {
                                start: n,
                                end: n + 1,
                            },
                            &text,
                        ),
                    },
                ));
            }
        }
        Ok(())
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("api/src")).unwrap();
    std::fs::create_dir_all(root.join("plain/src")).unwrap();
    std::fs::write(
        root.join("api/package.json"),
        r#"{"name":"api","dependencies":{"fakefw":"1"}}"#,
    )
    .unwrap();
    std::fs::write(root.join("plain/package.json"), r#"{"name":"plain"}"#).unwrap();
    std::fs::write(root.join("api/src/a.ts"), "export const a = 1;\n").unwrap();
    std::fs::write(root.join("api/src/b.ts"), "export const b = 'dynamic';\n").unwrap();
    std::fs::write(root.join("plain/src/c.ts"), "export const c = 1;\n").unwrap();
    dir
}

fn service(
    dir: &std::path::Path,
    adapter: FakeAdapter,
) -> crate::repository_intelligence::RepositoryGraphService {
    RepositoryGraphBuilder::new(std::fs::canonicalize(dir).unwrap())
        .unwrap()
        .framework_adapter(Arc::new(adapter))
        .build()
}

fn query(snapshot: &crate::repository_intelligence::SnapshotId, limit: usize) -> FrameworkQuery {
    FrameworkQuery {
        snapshot: snapshot.clone(),
        filter: FrameworkFilter::default(),
        cursor: None,
        limit,
    }
}

#[tokio::test]
async fn an_adapter_runs_only_where_the_framework_applies_and_leaves_evidence() {
    let dir = workspace();
    let service = service(
        dir.path(),
        FakeAdapter {
            version: 1,
            per_file: 1,
        },
    );
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    let index = &snapshot.frameworks;
    assert!(index.configured);
    assert_eq!(index.applications.len(), 1);
    assert_eq!(index.applications[0].framework, "fakefw");
    assert_eq!(
        index.applications[0].evidence,
        "declares dependency `fakefw`"
    );
    assert_eq!(
        index.relations.len(),
        1,
        "the plain package has no relations; b.ts is dynamic"
    );
    let relation = &index.relations[0];
    assert_eq!(
        relation.from,
        RootRelativePath::new("api/src/a.ts").unwrap()
    );
    assert_eq!(
        relation.rule,
        ComponentVersion {
            id: "fake-route".into(),
            version: 1
        }
    );
    assert_eq!(relation.certainty, Certainty::Declared);
    assert!(relation.evidence.text.contains("export const a"));
    assert!(relation.id.0.starts_with("rel:"));
    assert_eq!(
        index.diagnostics.len(),
        1,
        "the computed route is a diagnostic, not a relation"
    );
    assert!(index.diagnostics[0].message.contains("no relation emitted"));
}

#[tokio::test]
async fn without_adapters_nothing_is_configured_and_queries_say_so() {
    let dir = workspace();
    let service = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .build();
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    assert!(!snapshot.frameworks.configured);
    assert_eq!(
        service
            .framework_relations(&query(&snapshot.id, 10))
            .unwrap_err(),
        FrameworkQueryError::NotConfigured
    );
    assert!(service.framework_capabilities().is_empty());
}

#[tokio::test]
async fn the_snapshot_id_follows_adapter_version_and_relations_deterministically() {
    let dir = workspace();
    let id = |adapter: FakeAdapter| {
        let service = service(dir.path(), adapter);
        async move { service.refresh(CancellationToken::new()).await.unwrap().id }
    };
    let first = id(FakeAdapter {
        version: 1,
        per_file: 1,
    })
    .await;
    assert_eq!(
        first,
        id(FakeAdapter {
            version: 1,
            per_file: 1
        })
        .await
    );
    assert_ne!(
        first,
        id(FakeAdapter {
            version: 1,
            per_file: 2
        })
        .await,
        "different relations"
    );

    let plain = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .build()
        .refresh(CancellationToken::new())
        .await
        .unwrap()
        .id;
    assert_ne!(
        first, plain,
        "registering framework analysis changes the id"
    );
}

#[tokio::test]
async fn relations_per_file_are_bounded_and_reported() {
    let dir = workspace();
    let service = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .framework_analyzer(Arc::new(DefaultFrameworkAnalyzer::new(
            vec![Arc::new(FakeAdapter {
                version: 1,
                per_file: 5,
            })],
            FrameworkLimits {
                max_relations_per_file: 2,
                ..FrameworkLimits::default()
            },
        )))
        .build();
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.frameworks.relations.len(), 2);
    assert!(snapshot.frameworks.partial);
    assert!(snapshot
        .frameworks
        .diagnostics
        .iter()
        .any(|d| d.message.contains("exceed the limit of 2")));
}

#[tokio::test]
async fn queries_filter_page_and_reject_bad_input() {
    let dir = workspace();
    let service = service(
        dir.path(),
        FakeAdapter {
            version: 1,
            per_file: 3,
        },
    );
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();

    let page = service
        .framework_relations(&query(&snapshot.id, 2))
        .unwrap();
    assert_eq!(page.relations.len(), 2);
    let cursor = page.next_cursor.clone().unwrap();
    let rest = service
        .framework_relations(&FrameworkQuery {
            cursor: Some(cursor.clone()),
            ..query(&snapshot.id, 2)
        })
        .unwrap();
    assert_eq!(rest.relations.len(), 1);
    assert!(rest.next_cursor.is_none());

    let none = service
        .framework_relations(&FrameworkQuery {
            filter: FrameworkFilter {
                kind: Some(RelationKind::CommandExposedAs),
                ..Default::default()
            },
            ..query(&snapshot.id, 10)
        })
        .unwrap();
    assert!(none.relations.is_empty());
    let by_file = service
        .framework_relations(&FrameworkQuery {
            filter: FrameworkFilter {
                file: Some(RootRelativePath::new("api/src/a.ts").unwrap()),
                framework: Some("fakefw".into()),
                ..Default::default()
            },
            ..query(&snapshot.id, 10)
        })
        .unwrap();
    assert_eq!(by_file.relations.len(), 3);

    // A cursor belongs to one query shape and one snapshot; limits and snapshots are checked.
    let other_shape = FrameworkQuery {
        cursor: Some(cursor),
        filter: FrameworkFilter {
            kind: Some(RelationKind::RouteRegisteredTo),
            ..Default::default()
        },
        ..query(&snapshot.id, 2)
    };
    assert_eq!(
        service.framework_relations(&other_shape).unwrap_err(),
        FrameworkQueryError::InvalidCursor
    );
    assert!(matches!(
        service.framework_relations(&query(&snapshot.id, 0)),
        Err(FrameworkQueryError::InvalidLimit { .. })
    ));
    assert!(matches!(
        service.framework_relations(&query(
            &crate::repository_intelligence::SnapshotId("nope".into()),
            5
        )),
        Err(FrameworkQueryError::SnapshotNotFound(_))
    ));
}

#[tokio::test]
async fn capabilities_come_from_the_registered_adapters() {
    let dir = workspace();
    let service = service(
        dir.path(),
        FakeAdapter {
            version: 7,
            per_file: 1,
        },
    );
    let capabilities = service.framework_capabilities();
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].adapter_version, 7);
    assert_eq!(capabilities[0].unsupported, ["nothing else"]);
}

#[tokio::test]
async fn a_cancelled_refresh_publishes_nothing() {
    let dir = workspace();
    let service = service(
        dir.path(),
        FakeAdapter {
            version: 1,
            per_file: 1,
        },
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        service.refresh(cancel).await,
        Err(DiscoveryError::Cancelled)
    ));
    assert!(service.current().is_none());
}

#[test]
fn evidence_text_is_bounded() {
    let long = "x".repeat(500);
    let evidence = RelationEvidence::new(
        &RootRelativePath::new("a.ts").unwrap(),
        ByteSpan { start: 0, end: 500 },
        &long,
    );
    assert_eq!(evidence.text.chars().count(), EVIDENCE_TEXT_LIMIT + 1);
    assert!(evidence.text.ends_with('…'));
}

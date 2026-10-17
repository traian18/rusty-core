mod support;

use harness_workspace::import_graph::{
    ImportFilter, ImportForm, ImportKind, ImportReason, ItemStatus, Resolution, ResolutionKind,
    Staticness,
};
use harness_workspace::repository_intelligence::RepositoryGraph;
use support::*;
use tempfile::tempdir;

#[tokio::test]
async fn typescript_imports_resolve_with_workspace_packages_and_items() {
    let dir = tempdir().unwrap();
    ts_workspace(dir.path());
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let main = outgoing(&graph, &id, "packages/app/src/main.ts");

    // Named imports through a workspace package entry and its re-exports.
    let core = main
        .iter()
        .filter(|e| e.fact.spelling == "@acme/core")
        .collect::<Vec<_>>();
    let by_name = |name: &str| {
        core.iter()
            .find(|e| e.fact.imported_name.as_deref() == Some(name) && e.fact.alias.is_none())
            .copied()
            .unwrap_or_else(|| panic!("no import of {name}"))
    };
    for name in ["Button", "slug", "Props"] {
        match &by_name(name).resolution {
            Resolution::ResolvedInternal {
                target_file,
                item_status,
                via,
                ..
            } => {
                assert_eq!(
                    target_file.as_path().to_str(),
                    Some("packages/core/src/index.ts")
                );
                assert_eq!(*item_status, ItemStatus::Found, "{name}");
                assert_eq!(via.len(), 1, "{name}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(by_name("Button").rule, "ts.workspace_package_entry");
    assert!(matches!(
        &by_name("Missing").resolution,
        Resolution::ResolvedInternal {
            item_status: ItemStatus::NotFound,
            ..
        }
    ));
    assert!(by_name("Button")
        .to_package
        .as_ref()
        .unwrap()
        .0
        .ends_with("@acme/core"));

    // Type-only distinction, statement and specifier level.
    let p2 = core
        .iter()
        .find(|e| e.fact.alias.as_deref() == Some("P2"))
        .unwrap();
    assert_eq!(p2.fact.kind, ImportKind::TypeOnly);
    assert_eq!(by_name("Props").fact.kind, ImportKind::Runtime);
    let dir_edges = main
        .iter()
        .filter(|e| e.fact.spelling == "./dir")
        .collect::<Vec<_>>();
    assert_eq!(dir_edges.len(), 2);
    assert!(dir_edges.iter().any(
        |e| e.fact.imported_name.as_deref() == Some("C") && e.fact.kind == ImportKind::TypeOnly
    ));
    assert!(dir_edges
        .iter()
        .all(|e| target(e) == "packages/app/src/dir/index.ts"));

    // Relative probing, js->ts mapping, default/namespace/side-effect forms.
    let local = main
        .iter()
        .filter(|e| e.fact.spelling == "./local")
        .collect::<Vec<_>>();
    assert!(local
        .iter()
        .all(|e| target(e) == "packages/app/src/local.ts"));
    assert!(local
        .iter()
        .any(|e| e.fact.imported_name.as_deref() == Some("default")));
    assert!(local
        .iter()
        .any(|e| e.fact.imported_name.as_deref() == Some("*")));
    assert!(local
        .iter()
        .any(|e| e.fact.form == ImportForm::Require && e.fact.staticness == Staticness::Static));
    assert!(local
        .iter()
        .any(|e| e.fact.form == ImportForm::ExportAllFrom && e.fact.reexport));
    assert_eq!(
        target(edge(&main, "./local.js")),
        "packages/app/src/local.ts"
    );
    match &edge(&main, "./local.js").resolution {
        Resolution::ResolvedInternal {
            item_status,
            target_item,
            ..
        } => {
            assert_eq!(target_item.as_deref(), Some("x"));
            assert_eq!(*item_status, ItemStatus::Found);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        edge(&main, "./side-effect").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));

    // Subpath into a workspace package, externals, aliases, assets, dynamic forms.
    assert_eq!(
        edge(&main, "@acme/core/src/util").rule,
        "ts.workspace_package_subpath"
    );
    assert_eq!(
        target(edge(&main, "@acme/core/src/util")),
        "packages/core/src/util.ts"
    );
    assert!(
        matches!(&edge(&main, "lodash").resolution, Resolution::DeclaredExternal { package } if package == "lodash")
    );
    assert!(matches!(
        &edge(&main, "react").resolution,
        Resolution::DeclaredExternal { .. }
    ));
    assert_eq!(edge(&main, "node:fs").rule, "ts.node_builtin");
    assert!(matches!(
        edge(&main, "nope").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(matches!(
        edge(&main, "@/alias").resolution,
        Resolution::Unresolved {
            reason: ImportReason::AliasUnsupported
        }
    ));
    assert_eq!(edge(&main, "./logo.svg").rule, "ts.asset_import");
    let lazy = edge(&main, "./lazy");
    assert_eq!(lazy.fact.form, ImportForm::DynamicImport);
    assert_eq!(lazy.fact.staticness, Staticness::Dynamic);
    assert_eq!(target(lazy), "packages/app/src/lazy.ts");
    let non_literal = edge(&main, "someVar");
    assert!(matches!(
        non_literal.resolution,
        Resolution::DynamicOrUnsupported {
            reason: ImportReason::NonLiteralSpecifier
        }
    ));

    // A same-named file in another package is not selected.
    assert!(matches!(
        edge(&main, "./util").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    assert!(incoming(&graph, &id, "packages/core/src/util.ts")
        .iter()
        .all(
            |e| e.fact.importing_file.as_path().starts_with("packages/core")
                || e.fact.spelling == "@acme/core/src/util"
                || e.fact.spelling == "@lib/util"
        ));

    // TSX and JS files use the same resolution.
    let comp = outgoing(&graph, &id, "packages/app/src/comp.tsx");
    assert_eq!(target(edge(&comp, "./local")), "packages/app/src/local.ts");
    let legacy = outgoing(&graph, &id, "packages/app/src/legacy.js");
    assert_eq!(
        target(edge(&legacy, "./local")),
        "packages/app/src/local.ts"
    );

    // Exact byte spans for named specifiers.
    let source = std::fs::read_to_string(dir.path().join("packages/app/src/main.ts")).unwrap();
    let span = by_name("Button").fact.span;
    assert_eq!(&source[span.start..span.end], "Button");
    let span = edge(&main, "node:fs").fact.span;
    assert_eq!(&source[span.start..span.end], "fs");
}

#[tokio::test]
async fn queries_filter_page_and_reject_foreign_cursors() {
    let dir = tempdir().unwrap();
    ts_workspace(dir.path());
    let graph = service(dir.path());
    let id = refresh(&graph).await;

    let mut query = query(&id, "packages/app/src/local.ts", 2);
    let first = graph.dependents(&query).unwrap();
    assert_eq!(first.edges.len(), 2);
    let cursor = first.next_cursor.clone().expect("more pages");
    query.cursor = Some(cursor.clone());
    let second = graph.dependents(&query).unwrap();
    assert!(!second.edges.is_empty());
    assert!(second.edges.iter().all(|e| !first.edges.contains(e)));

    // Paging through everything returns each import site exactly once, in order.
    let all = incoming(&graph, &id, "packages/app/src/local.ts");
    let mut collected = Vec::new();
    let mut paged = support::query(&id, "packages/app/src/local.ts", 1);
    loop {
        let page = graph.dependents(&paged).unwrap();
        collected.extend(page.edges);
        match page.next_cursor {
            Some(next) => paged.cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, all);

    // Filters.
    let mut typed = support::query(&id, "packages/app/src/main.ts", 100);
    typed.filter = ImportFilter {
        kind: Some(ImportKind::TypeOnly),
        ..ImportFilter::default()
    };
    let only_types = graph.dependencies(&typed).unwrap().edges;
    assert!(
        !only_types.is_empty()
            && only_types
                .iter()
                .all(|e| e.fact.kind == ImportKind::TypeOnly)
    );
    typed.filter = ImportFilter {
        resolution: vec![ResolutionKind::DeclaredExternal],
        ..ImportFilter::default()
    };
    assert!(graph
        .dependencies(&typed)
        .unwrap()
        .edges
        .iter()
        .all(|e| e.resolution.kind() == ResolutionKind::DeclaredExternal));

    // Item-level dependents.
    let mut item = support::query(&id, "packages/app/src/local.ts", 100);
    item.item = Some("x".into());
    assert!(graph
        .dependents(&item)
        .unwrap()
        .edges
        .iter()
        .all(|e| e.fact.imported_name.as_deref() == Some("x")));

    // Typed errors instead of silent resets.
    let mut bad = support::query(&id, "packages/app/src/local.ts", 0);
    assert!(graph.dependents(&bad).is_err());
    bad.limit = 10_000;
    assert!(graph.dependents(&bad).is_err());
    let mut foreign = support::query(&id, "packages/app/src/local.ts", 1);
    foreign.cursor = Some(harness_workspace::import_graph::ImportCursor {
        snapshot: harness_workspace::repository_intelligence::SnapshotId("other".into()),
        fingerprint: cursor.fingerprint.clone(),
        offset: 1,
    });
    assert_eq!(
        graph.dependents(&foreign),
        Err(harness_workspace::import_graph::ImportQueryError::ForeignCursor)
    );
    let mut mismatched = support::query(&id, "packages/app/src/main.ts", 1);
    mismatched.cursor = Some(cursor);
    assert_eq!(
        graph.dependents(&mismatched),
        Err(harness_workspace::import_graph::ImportQueryError::InvalidCursor)
    );
    let missing = support::query(&id, "packages/app/src/nope.ts", 1);
    assert!(matches!(
        graph.dependencies(&missing),
        Err(harness_workspace::import_graph::ImportQueryError::UnknownFile(_))
    ));
    let mut unknown_snapshot = support::query(&id, "packages/app/src/local.ts", 1);
    unknown_snapshot.snapshot =
        harness_workspace::repository_intelligence::SnapshotId("gone".into());
    assert!(matches!(
        graph.dependents(&unknown_snapshot),
        Err(harness_workspace::import_graph::ImportQueryError::SnapshotNotFound(_))
    ));
}

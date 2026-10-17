mod support;

use harness_workspace::import_graph::{ImportReason, ItemStatus, Resolution};
use harness_workspace::repository_intelligence::RepositoryGraph;
use support::*;
use tempfile::tempdir;

#[tokio::test]
async fn rust_modules_items_and_crates_resolve_with_evidence() {
    let dir = tempdir().unwrap();
    rust_workspace(dir.path());
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let main = outgoing(&graph, &id, "app/src/main.rs");

    assert_eq!(target(edge(&main, "config")), "app/src/config.rs");
    assert_eq!(edge(&main, "config").rule, "rust.mod_decl");
    assert!(matches!(
        edge(&main, "missing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    assert_eq!(target(edge(&main, "odd")), "app/src/other/odd.rs");
    assert_eq!(edge(&main, "odd").rule, "rust.mod_path_attribute");

    // Item level through a sibling crate, with a re-export hop.
    let circle = edge(&main, "util::Circle");
    assert_eq!(circle.rule, "rust.workspace_sibling");
    match &circle.resolution {
        Resolution::ResolvedInternal {
            target_file,
            target_item,
            item_status,
            via,
            definition_file,
        } => {
            assert_eq!(target_file.as_path().to_str(), Some("util/src/lib.rs"));
            assert_eq!(target_item.as_deref(), Some("Circle"));
            assert_eq!(*item_status, ItemStatus::Found);
            assert_eq!(via.len(), 1);
            assert_eq!(
                definition_file.as_ref().unwrap().as_path().to_str(),
                Some("util/src/shapes.rs")
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        edge(&main, "util::shapes::Square")
            .fact
            .imported_name
            .as_deref(),
        Some("Square")
    );
    match &edge(&main, "util::shapes::Square").resolution {
        Resolution::ResolvedInternal {
            target_file,
            item_status,
            ..
        } => {
            assert_eq!(target_file.as_path().to_str(), Some("util/src/shapes.rs"));
            assert_eq!(*item_status, ItemStatus::Found);
        }
        other => panic!("{other:?}"),
    }
    // Item missing from an existing module stays a file edge.
    match &edge(&main, "util::shapes::Triangle").resolution {
        Resolution::ResolvedInternal {
            target_file,
            item_status,
            ..
        } => {
            assert_eq!(target_file.as_path().to_str(), Some("util/src/shapes.rs"));
            assert_eq!(*item_status, ItemStatus::NotFound);
        }
        other => panic!("{other:?}"),
    }
    // Glob re-export: `pub use nested::deep::*` makes `g` reachable from lib.rs.
    match &edge(&main, "util::g").resolution {
        Resolution::ResolvedInternal {
            item_status,
            via,
            definition_file,
            ..
        } => {
            assert_eq!(*item_status, ItemStatus::Found);
            assert_eq!(via.len(), 1);
            assert_eq!(
                definition_file.as_ref().unwrap().as_path().to_str(),
                Some("util/src/nested/deep.rs")
            );
        }
        other => panic!("{other:?}"),
    }
    match &edge(&main, "util::nofn").resolution {
        Resolution::ResolvedInternal { item_status, .. } => {
            assert_eq!(*item_status, ItemStatus::NotFound)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        target(edge(&main, "util::nested::deep::f")),
        "util/src/nested/deep.rs"
    );

    // Externals and unresolved crates.
    assert!(
        matches!(&edge(&main, "serde::Serialize").resolution, Resolution::DeclaredExternal { package } if package == "serde")
    );
    assert_eq!(
        edge(&main, "serde::Serialize").rule,
        "rust.declared_external"
    );
    assert_eq!(edge(&main, "std::fmt").rule, "rust.implicit_std");
    assert!(matches!(
        edge(&main, "unknown::X").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert_eq!(edge(&main, "serde").rule, "rust.extern_crate");

    // crate:: / self:: / glob / use inside a function.
    assert_eq!(
        edge(&main, "crate::config::Settings").rule,
        "rust.crate_root"
    );
    assert_eq!(
        target(edge(&main, "crate::config::Settings")),
        "app/src/config.rs"
    );
    let glob = edge(&main, "config");
    assert!(main
        .iter()
        .any(|e| e.fact.glob && e.fact.spelling == "config"));
    let _ = glob;
    assert_eq!(
        edge(&main, "self::config::Settings").rule,
        "rust.self_module"
    );

    // super:: from a module file.
    let config = outgoing(&graph, &id, "app/src/config.rs");
    assert_eq!(edge(&config, "super::odd").rule, "rust.super_module");
    assert_eq!(target(edge(&config, "super::odd")), "app/src/other/odd.rs");
    assert!(matches!(
        &edge(&config, "crate::missing_item::Y").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
}

#[tokio::test]
async fn cargo_target_roots_attach_test_modules_and_unattached_files_are_reported() {
    let dir = tempdir().unwrap();
    rust_workspace(dir.path());
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let it = outgoing(&graph, &id, "app/tests/it.rs");
    assert_eq!(target(edge(&it, "common")), "app/tests/common/mod.rs");
    assert_eq!(target(edge(&it, "util::helper")), "util/src/lib.rs");

    let stray = outgoing(&graph, &id, "app/src/stray.rs");
    assert!(matches!(
        edge(&stray, "crate::config").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UnattachedFile
        }
    ));
    assert_eq!(edge(&stray, "std::io").rule, "rust.implicit_std");
    let snapshot = graph.current().unwrap();
    assert!(snapshot.imports.diagnostics.iter().any(|d| d
        .path
        .as_ref()
        .is_some_and(|p| p.as_path().ends_with("stray.rs"))
        && d.message.contains("unattached")));
}

#[tokio::test]
async fn same_name_files_are_never_selected_by_name_alone() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(dir.path(), "src/lib.rs", "pub mod a;\npub mod b;\n");
    write(dir.path(), "src/a/mod.rs", "pub mod util;\n");
    write(dir.path(), "src/a/util.rs", "pub fn one() {}\n");
    write(
        dir.path(),
        "src/b/mod.rs",
        "pub mod util;\nuse super::a::util::one;\n",
    );
    write(dir.path(), "src/b/util.rs", "pub fn two() {}\n");
    write(dir.path(), "src/c/util.rs", "pub fn three() {}\n");
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let b = outgoing(&graph, &id, "src/b/mod.rs");
    assert_eq!(target(edge(&b, "util")), "src/b/util.rs");
    assert_eq!(target(edge(&b, "super::a::util::one")), "src/a/util.rs");
    assert!(incoming(&graph, &id, "src/c/util.rs").is_empty());
}

#[tokio::test]
async fn repeated_imports_keep_individual_sites_and_exact_spans() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    let source =
        "pub mod é;\nuse crate::é::A;\nuse crate::é::A;\npub fn f() { use crate::é::A as B; }\n";
    write(dir.path(), "src/lib.rs", source);
    write(dir.path(), "src/é.rs", "pub struct A;\n");
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let edges = outgoing(&graph, &id, "src/lib.rs");
    let sites = edges
        .iter()
        .filter(|e| e.fact.spelling == "crate::é::A")
        .collect::<Vec<_>>();
    assert_eq!(sites.len(), 3);
    for site in &sites {
        let span = site.fact.span;
        assert!(source[span.start..span.end].starts_with("crate::é::A"));
    }
    assert_eq!(
        sites
            .iter()
            .map(|e| e.fact.id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(sites[2].fact.alias.as_deref(), Some("B"));
    // Dependents see each site separately.
    assert_eq!(incoming(&graph, &id, "src/é.rs").len(), 4); // three uses + the mod declaration
}

#[tokio::test]
async fn unattached_files_are_recorded_as_data_not_only_diagnostics() {
    let dir = tempdir().unwrap();
    rust_workspace(dir.path());
    write(dir.path(), "app/src/silent.rs", "pub fn quiet() {}\n");
    let graph = service(dir.path());
    refresh(&graph).await;
    let snapshot = graph.current().unwrap();
    let unattached = snapshot
        .imports
        .unattached
        .iter()
        .map(|p| p.as_path().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(unattached, vec!["app/src/silent.rs", "app/src/stray.rs"]);
    assert!(
        snapshot
            .imports
            .files
            .iter()
            .any(|f| f.path == path("app/src/silent.rs") && f.fact_count == 0),
        "a file without any import is still detected"
    );
}

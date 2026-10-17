mod support;

use harness_workspace::import_graph::{ImportReason, ItemStatus, Resolution};
use harness_workspace::repository_intelligence::RepositoryGraph;
use support::*;
use tempfile::tempdir;

#[tokio::test]
async fn tsconfig_paths_and_base_url_resolve_with_config_evidence() {
    let dir = tempdir().unwrap();
    ts_workspace(dir.path());
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    let main = outgoing(&graph, &id, "packages/app/src/main.ts");

    // Wildcard `paths` through an inherited, comment-laden config.
    let slug = edge(&main, "@lib/util");
    assert_eq!(slug.rule, "ts.tsconfig_paths");
    assert_eq!(target(slug), "packages/core/src/util.ts");
    assert_eq!(
        slug.evidence,
        vec![
            path("packages/app/tsconfig.json"),
            path("tsconfig.base.json")
        ]
    );
    match &slug.resolution {
        Resolution::ResolvedInternal {
            item_status,
            target_item,
            ..
        } => {
            assert_eq!(target_item.as_deref(), Some("slug"));
            assert_eq!(*item_status, ItemStatus::Found);
        }
        other => panic!("{other:?}"),
    }

    // Exact pattern pointing at a re-exporting entry still finds the item through the chain.
    match &edge(&main, "@acme/ui").resolution {
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
            assert_eq!(*item_status, ItemStatus::Found);
            assert_eq!(via.len(), 1);
        }
        other => panic!("{other:?}"),
    }

    // A matched pattern whose targets do not exist is a path failure, not an undeclared package.
    let missing = edge(&main, "@missing/thing");
    assert!(matches!(
        missing.resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    assert_eq!(missing.rule, "ts.tsconfig_paths");

    // Unmatched alias-looking specifiers stay explicitly unsupported.
    assert!(matches!(
        edge(&main, "@/alias").resolution,
        Resolution::Unresolved {
            reason: ImportReason::AliasUnsupported
        }
    ));

    // `baseUrl` without `paths`.
    let lib2 = outgoing(&graph, &id, "packages/lib2/src/main.ts");
    let helper = edge(&lib2, "shared/helper");
    assert_eq!(helper.rule, "ts.tsconfig_base_url");
    assert_eq!(target(helper), "packages/lib2/src/shared/helper.ts");
    assert_eq!(helper.evidence, vec![path("packages/lib2/tsconfig.json")]);

    // Configs are part of the index and of the snapshot identity.
    let snapshot = graph.current().unwrap();
    assert_eq!(snapshot.imports.configs.len(), 3);
    write(
        dir.path(),
        "tsconfig.base.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{}}}"#,
    );
    let changed = refresh(&graph).await;
    assert_ne!(id, changed);
    assert!(matches!(
        edge(
            &outgoing(&graph, &changed, "packages/app/src/main.ts"),
            "@lib/util"
        )
        .resolution,
        Resolution::Unresolved { .. }
    ));
}

#[tokio::test]
async fn tsconfig_precedence_nearest_config_and_malformed_input() {
    let dir = tempdir().unwrap();
    write(dir.path(), "package.json", r#"{"name":"root"}"#);
    write(
        dir.path(),
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"],"@/special/*":["special/*"],"exact":["src/exact.ts"]}}}"#,
    );
    write(dir.path(), "src/a.ts", "import a from '@/other';\nimport b from '@/special/thing';\nimport c from 'exact';\nimport d from 'src/other';\n");
    write(dir.path(), "src/other.ts", "export default 1;\n");
    write(dir.path(), "src/exact.ts", "export default 1;\n");
    write(dir.path(), "special/thing.ts", "export default 1;\n");
    // A nested directory with its own config overrides the root mappings entirely.
    write(
        dir.path(),
        "nested/tsconfig.json",
        r#"{"compilerOptions":{"paths":{"@/*":["lib/*"]}}}"#,
    );
    write(dir.path(), "nested/lib/x.ts", "export default 1;\n");
    write(
        dir.path(),
        "nested/b.ts",
        "import x from '@/x';\nimport y from 'exact';\n",
    );
    // jsconfig governs JavaScript when there is no tsconfig.
    write(
        dir.path(),
        "js/jsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"."}}"#,
    );
    write(dir.path(), "js/util.js", "module.exports = 1;\n");
    write(
        dir.path(),
        "js/main.js",
        "const u = require('util');\nimport v from 'util/../util';\n",
    );
    // Malformed and unresolvable extends are diagnostics, not failures.
    write(
        dir.path(),
        "bad/tsconfig.json",
        r#"{"extends": "@tsconfig/node18/tsconfig.json", "compilerOptions": {"paths": {"~/*": ["*"]}}"#,
    );
    write(dir.path(), "bad/c.ts", "import z from '~/z';\n");

    let graph = service(dir.path());
    let id = refresh(&graph).await;

    let a = outgoing(&graph, &id, "src/a.ts");
    assert_eq!(target(edge(&a, "@/other")), "src/other.ts");
    // The longer wildcard prefix wins over the shorter one.
    assert_eq!(target(edge(&a, "@/special/thing")), "special/thing.ts");
    assert_eq!(target(edge(&a, "exact")), "src/exact.ts");
    assert_eq!(edge(&a, "src/other").rule, "ts.tsconfig_base_url");

    let b = outgoing(&graph, &id, "nested/b.ts");
    assert_eq!(target(edge(&b, "@/x")), "nested/lib/x.ts");
    assert!(matches!(
        edge(&b, "exact").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));

    let js = outgoing(&graph, &id, "js/main.js");
    // `util` is also a Node builtin; the builtin wins, as it does at runtime.
    assert_eq!(js.iter().filter(|e| e.fact.spelling == "util").count(), 1);
    assert_eq!(edge(&js, "util").rule, "ts.node_builtin");
    assert_eq!(edge(&js, "util/../util").rule, "ts.node_builtin");

    let snapshot = graph.current().unwrap();
    assert!(snapshot
        .imports
        .diagnostics
        .iter()
        .any(|d| d.message.contains("malformed tsconfig.json")));
    let c = outgoing(&graph, &id, "bad/c.ts");
    assert!(matches!(
        edge(&c, "~/z").resolution,
        Resolution::Unresolved { .. }
    ));
}

#[tokio::test]
async fn unresolvable_extends_is_reported_and_other_options_still_apply() {
    let dir = tempdir().unwrap();
    write(dir.path(), "package.json", r#"{"name":"root"}"#);
    write(
        dir.path(),
        "tsconfig.json",
        r#"{"extends":"@tsconfig/node18/tsconfig.json","compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
    );
    write(dir.path(), "src/a.ts", "import o from '@/o';\n");
    write(dir.path(), "src/o.ts", "export default 1;\n");
    let graph = service(dir.path());
    let id = refresh(&graph).await;
    assert_eq!(
        target(edge(&outgoing(&graph, &id, "src/a.ts"), "@/o")),
        "src/o.ts"
    );
    assert!(graph
        .current()
        .unwrap()
        .imports
        .diagnostics
        .iter()
        .any(|d| d.message.contains("not a relative config")));
}

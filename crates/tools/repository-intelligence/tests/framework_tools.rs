mod support;

use std::sync::Arc;

use harness_repository_frameworks::ExpressAdapter;
use harness_repository_imports::{TypeScriptImportAdapter, TypeScriptImportResolver};
use harness_tool_repository_intelligence::RepositoryIntelligence;
use harness_workspace::{
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use serde_json::json;
use support::*;

fn graph(root: &std::path::Path, with_express: bool) -> Arc<dyn RepositoryGraph> {
    let mut builder = RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(TypeScriptImportAdapter),
            resolver: Arc::new(TypeScriptImportResolver),
        })
        .unwrap();
    if with_express {
        builder = builder.framework_adapter(Arc::new(ExpressAdapter));
    }
    Arc::new(builder.build())
}

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"name":"api","dependencies":{"express":"4"}}"#,
    );
    write(
        dir.path(),
        "src/app.ts",
        "import express from 'express';\nimport { list } from './users';\nconst app = express();\napp.get('/users', list);\napp.post('/users', list);\napp.delete('/users/:id', list);\nexport default app;\n",
    );
    write(dir.path(), "src/users.ts", "export function list() {}\n");
    dir
}

#[tokio::test]
async fn framework_relations_are_listed_with_evidence_and_the_shared_envelope() {
    let dir = repository();
    let tools = RepositoryIntelligence::new().with_graph(graph(dir.path(), true));
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (output, is_error) = call(&tools, "repo_graph.framework_relations", json!({})).await;
    assert!(!is_error, "{output}");
    assert_eq!(output["schema_version"], 1);
    let relations = output["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 3);
    let first = &relations[0];
    assert_eq!(first["kind"], "route_registered_to");
    assert_eq!(first["framework"], "express");
    assert_eq!(first["certainty"], "declared");
    assert_eq!(first["rule"], json!({"id": "express-route", "version": 1}));
    assert_eq!(first["label"], "GET /users");
    assert_eq!(first["to"], "src/users.ts");
    assert_eq!(first["evidence"]["text"], "app.get('/users', list)");
    assert!(output["component_versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "framework:express"));
    assert_eq!(output["result"]["applications"][0]["framework"], "express");
    assert_eq!(output["result"]["diagnostics"], 0);
}

#[tokio::test]
async fn framework_relations_filter_and_page_with_opaque_cursors() {
    let dir = repository();
    let tools = RepositoryIntelligence::new().with_graph(graph(dir.path(), true));
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (page, _) = call(
        &tools,
        "repo_graph.framework_relations",
        json!({"limit": 2}),
    )
    .await;
    assert_eq!(page["result"]["relations"].as_array().unwrap().len(), 2);
    let cursor = page["result"]["next_cursor"].as_str().unwrap().to_owned();
    let (rest, _) = call(
        &tools,
        "repo_graph.framework_relations",
        json!({"limit": 2, "cursor": cursor}),
    )
    .await;
    assert_eq!(rest["result"]["relations"].as_array().unwrap().len(), 1);
    assert!(rest["result"]["next_cursor"].is_null());

    let (by_file, _) = call(
        &tools,
        "repo_graph.framework_relations",
        json!({"file": "src/users.ts"}),
    )
    .await;
    assert_eq!(
        by_file["result"]["relations"].as_array().unwrap().len(),
        3,
        "relations that end in the file match too"
    );
    let (none, _) = call(
        &tools,
        "repo_graph.framework_relations",
        json!({"kind": "command_exposed_as"}),
    )
    .await;
    assert!(none["result"]["relations"].as_array().unwrap().is_empty());
    let (other, _) = call(
        &tools,
        "repo_graph.framework_relations",
        json!({"framework": "axum"}),
    )
    .await;
    assert!(other["result"]["relations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn capabilities_report_each_adapter_and_where_it_applies() {
    let dir = repository();
    let tools = RepositoryIntelligence::new().with_graph(graph(dir.path(), true));
    call(&tools, "repo_graph.refresh", json!({})).await;
    let (output, _) = call(&tools, "repo_graph.capabilities", json!({})).await;
    let frameworks = &output["result"]["frameworks"];
    assert_eq!(frameworks["adapters"][0]["framework"], "express");
    assert!(
        frameworks["adapters"][0]["unsupported"]
            .as_array()
            .unwrap()
            .len()
            >= 3
    );
    assert_eq!(
        frameworks["applications"][0]["evidence"],
        "declares dependency `express`"
    );
}

#[tokio::test]
async fn errors_are_structured_and_nothing_is_reported_without_adapters() {
    let dir = repository();
    let off = RepositoryIntelligence::new().with_graph(graph(dir.path(), false));
    call(&off, "repo_graph.refresh", json!({})).await;
    let (output, is_error) = call(&off, "repo_graph.framework_relations", json!({})).await;
    assert!(is_error);
    assert_eq!(error_code(&output), "not_configured");
    let (capabilities, _) = call(&off, "repo_graph.capabilities", json!({})).await;
    assert!(capabilities["result"]["frameworks"].is_null());

    let tools = RepositoryIntelligence::new().with_graph(graph(dir.path(), true));
    let (before, is_error) = call(&tools, "repo_graph.framework_relations", json!({})).await;
    assert!(is_error);
    assert_eq!(error_code(&before), "snapshot_not_found");
    call(&tools, "repo_graph.refresh", json!({})).await;
    for (arguments, code) in [
        (json!({"limit": 0}), "limit_exceeded"),
        (json!({"cursor": "not-a-cursor"}), "invalid_cursor"),
        (json!({"snapshot": "nope"}), "snapshot_not_found"),
        (json!({"file": "/etc/passwd"}), "invalid_request"),
        (json!({"surprise": true}), "invalid_request"),
        (json!({"kind": "teleport"}), "invalid_request"),
    ] {
        let (output, is_error) =
            call(&tools, "repo_graph.framework_relations", arguments.clone()).await;
        assert!(is_error, "{arguments}");
        assert_eq!(error_code(&output), code, "{arguments}: {output}");
    }
}

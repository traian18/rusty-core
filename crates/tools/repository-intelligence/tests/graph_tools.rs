mod support;

use serde_json::json;
use support::*;

#[tokio::test]
async fn refresh_publishes_a_snapshot_and_reports_the_envelope() {
    let fixture = fixture();
    let tools = fixture.graph_only();
    let (output, is_error) = call(&tools, "repo_graph.refresh", json!({})).await;
    assert!(!is_error, "{output}");
    assert_eq!(output["schema_version"], 1);
    assert!(output["snapshot"].as_str().unwrap().len() > 10);
    assert_eq!(output["partial"], false);
    assert!(output["truncation"].is_null());
    let result = &output["result"];
    assert_eq!(result["packages"], 2);
    assert_eq!(result["files"], 6);
    assert_eq!(result["groups"]["configured"], true);
    let versions = output["component_versions"].as_array().unwrap();
    assert!(versions.iter().any(|v| v["id"] == "adapter:rust-imports"));
    assert!(versions
        .iter()
        .any(|v| v["id"] == "group-rule:import-cycle"));
    // The tool published through the shared graph instance.
    assert_eq!(fixture.graph.current().unwrap().id.0, output["snapshot"]);
}

#[tokio::test]
async fn capabilities_lists_languages_rules_and_group_kinds() {
    let fixture = fixture();
    let tools = fixture.all();
    call(&tools, "repo_graph.refresh", json!({})).await;
    let (output, is_error) = call(&tools, "repo_graph.capabilities", json!({})).await;
    assert!(!is_error);
    let result = &output["result"];
    assert_eq!(result["imports"]["languages"].as_array().unwrap().len(), 2);
    assert_eq!(result["groups"]["kinds"].as_array().unwrap().len(), 4);
    assert_eq!(result["git_available"], true);
    assert_eq!(result["discovery"]["cargo"], true);
}

#[tokio::test]
async fn dependencies_and_dependents_page_with_opaque_cursors() {
    let fixture = fixture();
    let tools = fixture.graph_only();
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (lib, _) = call(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/lib.rs", "limit": 2}),
    )
    .await;
    assert_eq!(lib["result"]["edges"].as_array().unwrap().len(), 2);
    let cursor = lib["result"]["next_cursor"].as_str().unwrap().to_owned();
    let (rest, _) = call(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/lib.rs", "limit": 2, "cursor": cursor}),
    )
    .await;
    assert_eq!(rest["result"]["edges"].as_array().unwrap().len(), 1);
    assert!(rest["result"]["next_cursor"].is_null());

    let (dependents, is_error) = call(
        &tools,
        "repo_graph.dependents",
        json!({"file": "src/a.rs", "limit": 10}),
    )
    .await;
    assert!(!is_error);
    let importers = dependents["result"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["fact"]["importing_file"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(importers, vec!["src/b.rs", "src/c.rs", "src/lib.rs"]);

    // Filters narrow the answer and item scopes dependents to one item.
    let (external, _) = call(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/c.rs", "resolutions": ["declared_external"]}),
    )
    .await;
    assert!(external["result"]["edges"].as_array().unwrap().is_empty());
    let (item, _) = call(
        &tools,
        "repo_graph.dependents",
        json!({"file": "src/a.rs", "item": "A"}),
    )
    .await;
    assert_eq!(item["result"]["edges"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn overview_and_groups_answer_structure_questions() {
    let fixture = fixture();
    let tools = fixture.graph_only();
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (overview, is_error) = call(
        &tools,
        "repo_graph.overview",
        json!({"kind": "declared_package"}),
    )
    .await;
    assert!(!is_error);
    assert_eq!(overview["result"]["nodes"].as_array().unwrap().len(), 2);

    let (cycles, _) = call(
        &tools,
        "repo_graph.groups",
        json!({"action": "list", "kind": "import_cycle"}),
    )
    .await;
    let groups = cycles["result"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["id"], "import_cycle:src/a.rs");
    assert!(groups[0]["explanation"]
        .as_str()
        .unwrap()
        .contains("not intent"));

    let (members, _) = call(
        &tools,
        "repo_graph.groups",
        json!({"action": "members", "group": "import_cycle:src/a.rs"}),
    )
    .await;
    assert_eq!(
        members["result"]["members"],
        json!(["src/a.rs", "src/b.rs"])
    );

    let (of_file, _) = call(
        &tools,
        "repo_graph.groups",
        json!({"action": "of_file", "file": "src/a.rs"}),
    )
    .await;
    assert_eq!(
        of_file["result"]["assignments"].as_array().unwrap().len(),
        3
    );

    let (boundary, _) = call(
        &tools,
        "repo_graph.groups",
        json!({"action": "boundary", "group": "import_cycle:src/a.rs", "direction": "incoming"}),
    )
    .await;
    let spellings = boundary["result"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["fact"]["spelling"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(spellings, vec!["crate::a::A", "a", "b"]);

    let (missing, is_error) = call(&tools, "repo_graph.groups", json!({"action": "members"})).await;
    assert!(is_error);
    assert_eq!(error_code(&missing), "invalid_request");
}

#[tokio::test]
async fn explain_reports_direct_imports_paths_shared_groups_and_absence() {
    let fixture = fixture();
    let tools = fixture.graph_only();
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (output, is_error) = call(
        &tools,
        "repo_graph.explain",
        json!({"from": "src/a.rs", "to": "src/b.rs"}),
    )
    .await;
    assert!(!is_error, "{output}");
    let result = &output["result"];
    assert_eq!(result["conclusion"], "dependency_path_found");
    assert_eq!(result["from_depends_on_to"]["length"], 1);
    assert_eq!(result["to_depends_on_from"]["length"], 1);
    assert_eq!(
        result["direct"]["from_imports_to"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result["direct"]["from_imports_to"][0]["rule"],
        "rust.crate_root"
    );
    let shared = result["shared_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["kind"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        shared,
        vec!["declared_package", "source_root", "import_cycle"]
    );

    // c depends on b only through a: a two-hop path, and nothing the other way.
    let (path, _) = call(
        &tools,
        "repo_graph.explain",
        json!({"from": "src/c.rs", "to": "src/b.rs"}),
    )
    .await;
    assert_eq!(
        path["result"]["from_depends_on_to"]["files"],
        json!(["src/c.rs", "src/a.rs", "src/b.rs"])
    );
    assert!(path["result"]["to_depends_on_from"].is_null());
    assert!(path["result"]["direct"]["from_imports_to"]
        .as_array()
        .unwrap()
        .is_empty());

    // Unrelated files: absence is stated, not guessed.
    let (none, _) = call(
        &tools,
        "repo_graph.explain",
        json!({"from": "web/src/x.ts", "to": "src/a.rs"}),
    )
    .await;
    assert_eq!(
        none["result"]["conclusion"],
        "no_dependency_path_within_bounds"
    );

    let (bad, is_error) = call(
        &tools,
        "repo_graph.explain",
        json!({"from": "src/a.rs", "to": "nope.rs"}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&bad), "unknown_file");
    let (deep, is_error) = call(
        &tools,
        "repo_graph.explain",
        json!({"from": "src/a.rs", "to": "src/b.rs", "max_depth": 99}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&deep), "limit_exceeded");
}

#[tokio::test]
async fn impact_is_bounded_reports_truncation_and_keeps_git_hints_separate() {
    let fixture = fixture();
    let tools = fixture.all();
    call(&tools, "repo_graph.refresh", json!({})).await;

    let (output, is_error) = call(
        &tools,
        "repo_graph.impact",
        json!({"files": ["src/a.rs"], "depth": 2}),
    )
    .await;
    assert!(!is_error, "{output}");
    let result = &output["result"];
    assert_eq!(result["direction"], "dependents");
    assert_eq!(result["affected_files"], 3);
    assert_eq!(result["by_package"]["Cargo.toml:app"], 3);
    assert!(result.get("co_change_hints").is_none());
    assert!(output["truncation"].is_null());

    let (limited, _) = call(
        &tools,
        "repo_graph.impact",
        json!({"files": ["src/a.rs"], "node_limit": 2}),
    )
    .await;
    assert_eq!(limited["truncation"], "node_limit");

    let (shallow, _) = call(
        &tools,
        "repo_graph.impact",
        json!({"files": ["src/c.rs"], "direction": "dependencies", "depth": 1}),
    )
    .await;
    assert_eq!(shallow["truncation"], "depth_limit");

    // Co-change hints need a Git snapshot, are labelled, and never mix into the import results.
    let (before, is_error) = call(
        &tools,
        "repo_graph.impact",
        json!({"files": ["src/a.rs"], "include_co_change": true}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&before), "not_configured");
    call(&tools, "repo_git.refresh", json!({})).await;
    let (with_hints, is_error) = call(
        &tools,
        "repo_graph.impact",
        json!({"files": ["src/a.rs"], "include_co_change": true}),
    )
    .await;
    assert!(!is_error, "{with_hints}");
    assert!(with_hints["git_snapshot"].is_string());
    let hints = &with_hints["result"]["co_change_hints"];
    assert_eq!(hints["label"], "historical correlation, not a dependency");
    assert_eq!(hints["files"][0]["partners"][0]["partner"], "src/b.rs");

    let (graph_only, is_error) = call(
        &fixture.graph_only(),
        "repo_graph.impact",
        json!({"files": ["src/a.rs"], "include_co_change": true}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&graph_only), "not_configured");
}

#[tokio::test]
async fn invalid_input_missing_snapshots_and_cancellation_are_structured_errors() {
    let fixture = fixture();
    let tools = fixture.graph_only();

    // No snapshot published yet.
    let (output, is_error) = call(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/a.rs"}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&output), "snapshot_not_found");
    assert_eq!(output["schema_version"], 1);

    call(&tools, "repo_graph.refresh", json!({})).await;
    for (tool, arguments, code) in [
        (
            "repo_graph.dependencies",
            json!({"file": "/etc/passwd"}),
            "invalid_request",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "../outside.rs"}),
            "invalid_request",
        ),
        ("repo_graph.dependencies", json!({}), "invalid_request"),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "unexpected": 1}),
            "invalid_request",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "item": "A"}),
            "invalid_request",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "limit": 0}),
            "limit_exceeded",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "limit": 100000}),
            "limit_exceeded",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "cursor": "not-a-cursor"}),
            "invalid_cursor",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "nope.rs"}),
            "unknown_file",
        ),
        (
            "repo_graph.dependencies",
            json!({"file": "src/a.rs", "snapshot": "missing"}),
            "snapshot_not_found",
        ),
        (
            "repo_graph.groups",
            json!({"action": "members", "group": "nope"}),
            "unknown_group",
        ),
        (
            "repo_graph.groups",
            json!({"action": "bogus"}),
            "invalid_request",
        ),
        (
            "repo_graph.overview",
            json!({"kind": "declared_package", "node_limit": 0}),
            "limit_exceeded",
        ),
        ("repo_graph.impact", json!({"files": []}), "invalid_request"),
        (
            "repo_graph.impact",
            json!({"files": ["src/a.rs"], "depth": 9}),
            "limit_exceeded",
        ),
        (
            "repo_graph.refresh",
            json!({"max_files": 3}),
            "invalid_request",
        ),
    ] {
        let (output, is_error) = call(&tools, tool, arguments.clone()).await;
        assert!(is_error, "{tool} {arguments}: {output}");
        assert_eq!(error_code(&output), code, "{tool} {arguments}: {output}");
    }

    // A foreign-snapshot cursor is rejected rather than silently restarted.
    let (page, _) = call(
        &tools,
        "repo_graph.dependents",
        json!({"file": "src/a.rs", "limit": 1}),
    )
    .await;
    let cursor = page["result"]["next_cursor"].as_str().unwrap().to_owned();
    let (mismatch, is_error) = call(
        &tools,
        "repo_graph.dependents",
        json!({"file": "src/b.rs", "limit": 1, "cursor": cursor}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&mismatch), "invalid_cursor");

    // Cancellation before the call is a structured error, and refresh publishes nothing.
    let cancel = harness_tools::CancellationToken::new();
    cancel.cancel();
    let before = fixture.graph.current().unwrap().id.clone();
    let (cancelled, is_error) =
        call_with(&tools, "repo_graph.refresh", json!({}), cancel.clone()).await;
    assert!(is_error);
    assert_eq!(error_code(&cancelled), "cancelled");
    let (cancelled, _) = call_with(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/a.rs"}),
        cancel,
    )
    .await;
    assert_eq!(error_code(&cancelled), "cancelled");
    assert_eq!(fixture.graph.current().unwrap().id, before);
}

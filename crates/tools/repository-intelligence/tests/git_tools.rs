mod support;

use serde_json::json;
use support::*;

#[tokio::test]
async fn refresh_joins_git_facts_to_the_current_repository_snapshot() {
    let fixture = fixture();
    let tools = fixture.all();
    let (graph, _) = call(&tools, "repo_graph.refresh", json!({})).await;
    let (output, is_error) = call(&tools, "repo_git.refresh", json!({})).await;
    assert!(!is_error, "{output}");
    assert_eq!(output["schema_version"], 1);
    assert!(output["snapshot"].is_null());
    assert!(output["git_snapshot"].is_string());
    assert_eq!(output["result"]["repository_snapshot"], graph["snapshot"]);
    assert_eq!(
        output["result"]["files"], 8,
        "the first commit added every file"
    );
    assert_eq!(output["partial"], false);
    assert!(output["result"]["coverage"]["interpretation"]
        .as_str()
        .unwrap()
        .contains("not show causation"));
    assert_eq!(output["component_versions"][0]["id"], "git-analysis");
}

#[tokio::test]
async fn churn_co_change_contributors_and_coverage_carry_the_correlation_wording() {
    let fixture = fixture();
    let tools = fixture.all();
    call(&tools, "repo_git.refresh", json!({})).await;

    let (churn, is_error) = call(&tools, "repo_git.churn", json!({"sort": "path"})).await;
    assert!(!is_error);
    let entries = churn["result"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 8);
    let a = entries.iter().find(|e| e["file"] == "src/a.rs").unwrap();
    let c = entries.iter().find(|e| e["file"] == "src/c.rs").unwrap();
    assert_eq!(
        (a["commits"].as_u64(), c["commits"].as_u64()),
        (Some(4), Some(1))
    );
    assert!(churn["result"]["interpretation"]
        .as_str()
        .unwrap()
        .contains("not causation"));

    let (one, _) = call(
        &tools,
        "repo_git.churn",
        json!({"scope": "file", "path": "src/a.rs"}),
    )
    .await;
    assert_eq!(one["result"]["entries"].as_array().unwrap().len(), 1);
    let (prefix, _) = call(
        &tools,
        "repo_git.churn",
        json!({"scope": "prefix", "path": "src"}),
    )
    .await;
    assert_eq!(prefix["result"]["entries"].as_array().unwrap().len(), 4);

    let (partners, is_error) =
        call(&tools, "repo_git.co_changed", json!({"file": "src/a.rs"})).await;
    assert!(!is_error);
    let top = &partners["result"]["entries"][0];
    assert_eq!(
        (
            top["partner"].as_str().unwrap(),
            top["support"].as_u64().unwrap(),
            top["confidence_per_mille"].as_u64().unwrap()
        ),
        ("src/b.rs", 4, 1000)
    );
    assert!(partners["result"]["interpretation"]
        .as_str()
        .unwrap()
        .contains("not causation"));

    let (people, _) = call(&tools, "repo_git.contributors", json!({"file": "src/a.rs"})).await;
    assert_eq!(
        people["result"]["entries"][0]["contributor"]["name"],
        "Alice"
    );
    assert!(people["result"]["definition"]
        .as_str()
        .unwrap()
        .contains("not code ownership"));
    assert!(
        !people.to_string().contains("example.com"),
        "email addresses must not be exposed"
    );

    let (coverage, _) = call(&tools, "repo_git.coverage", json!({})).await;
    assert_eq!(coverage["result"]["commits_used"], 4);
    assert_eq!(coverage["result"]["walk_stop"], "exhausted");
}

#[tokio::test]
async fn git_errors_are_structured_and_the_toolsets_are_independent() {
    let fixture = fixture();
    let tools = fixture.all();

    let (none, is_error) = call(&tools, "repo_git.churn", json!({})).await;
    assert!(is_error);
    assert_eq!(error_code(&none), "snapshot_not_found");

    call(&tools, "repo_git.refresh", json!({})).await;
    for (tool, arguments, code) in [
        (
            "repo_git.co_changed",
            json!({"file": "src/zzz.rs"}),
            "unknown_file",
        ),
        (
            "repo_git.co_changed",
            json!({"file": "src/a.rs", "min_support": 1}),
            "invalid_request",
        ),
        (
            "repo_git.co_changed",
            json!({"file": "src/a.rs", "limit": 0}),
            "limit_exceeded",
        ),
        (
            "repo_git.churn",
            json!({"cursor": "garbage"}),
            "invalid_cursor",
        ),
        (
            "repo_git.churn",
            json!({"scope": "file"}),
            "invalid_request",
        ),
        (
            "repo_git.churn",
            json!({"git_snapshot": "gone"}),
            "snapshot_not_found",
        ),
        ("repo_git.contributors", json!({}), "invalid_request"),
        (
            "repo_git.coverage",
            json!({"extra": true}),
            "invalid_request",
        ),
    ] {
        let (output, is_error) = call(&tools, tool, arguments.clone()).await;
        assert!(is_error, "{tool} {arguments}");
        assert_eq!(error_code(&output), code, "{tool} {arguments}: {output}");
    }

    let cancel = harness_tools::CancellationToken::new();
    cancel.cancel();
    let (cancelled, is_error) = call_with(&tools, "repo_git.refresh", json!({}), cancel).await;
    assert!(is_error);
    assert_eq!(error_code(&cancelled), "cancelled");
}

#[tokio::test]
async fn toolsets_are_disabled_unless_their_dependency_is_supplied() {
    let fixture = fixture();
    let nothing = harness_tool_repository_intelligence::RepositoryIntelligence::new();
    assert!(nothing.executors().is_empty());
    assert!(nothing.executor_for("repo_graph.refresh").is_none());
    assert!(nothing.executor_for("repo_git.refresh").is_none());

    let graph_only = fixture.graph_only();
    assert_eq!(graph_only.executors().len(), 11);
    assert!(
        graph_only.executor_for("repo_git.churn").is_none(),
        "Git needs its own opt-in"
    );

    let git_only = harness_tool_repository_intelligence::RepositoryIntelligence::new()
        .with_git(fixture.git.clone());
    assert_eq!(git_only.executors().len(), 5);
    assert!(git_only.executor_for("repo_graph.refresh").is_none());

    let all = fixture.all();
    let names = all
        .executors()
        .iter()
        .map(|e| e.descriptor().name)
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 16);
    assert!(names
        .iter()
        .all(|n| harness_tool_repository_intelligence::is_repository_tool(n)));

    // Unknown names stay unknown, in both toolsets.
    assert!(all.executor_for("repo_graph.nonexistent").is_none());
    assert!(all.executor_for("code_graph.refresh").is_none());
    assert!(!harness_tool_repository_intelligence::is_repository_tool(
        "fs.read"
    ));
}

#[tokio::test]
async fn descriptors_carry_schemas_that_reject_unknown_fields() {
    let fixture = fixture();
    for executor in fixture.all().executors() {
        let descriptor = executor.descriptor();
        assert_eq!(
            descriptor.input_schema["type"], "object",
            "{}",
            descriptor.name
        );
        assert_eq!(
            descriptor.input_schema["additionalProperties"], false,
            "{}",
            descriptor.name
        );
        assert!(!descriptor.description.is_empty());
    }
}

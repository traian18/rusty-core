mod support;

use serde_json::{json, Value};
use support::*;

async fn ready() -> (
    Fixture,
    harness_tool_repository_intelligence::RepositoryIntelligence,
) {
    let fixture = fixture();
    write(fixture.dir.path(), "scripts/tool.rb", "puts 1\n");
    let tools = fixture.everything();
    call(&tools, "repo_graph.refresh", json!({})).await;
    call(&tools, "repo_git.refresh", json!({})).await;
    call(&tools, "repo_findings.analyze", json!({})).await;
    (fixture, tools)
}

fn result(output: &Value) -> &Value {
    &output["result"]
}

#[tokio::test]
async fn a_page_comes_back_as_markdown_with_citations_and_a_next_call() {
    let (_fixture, tools) = ready().await;
    let (output, is_error) = call(&tools, "repo_wiki", json!({"target": "file:src/a.rs"})).await;
    assert!(!is_error, "{output}");
    let page = result(&output);
    assert_eq!(page["status"], "ok");
    assert_eq!(page["level"], "file");
    let markdown = page["markdown"].as_str().unwrap();
    assert!(
        markdown.contains("## Imports") && markdown.contains("[src/a.rs:1-1]"),
        "{markdown}"
    );
    assert!(markdown.contains("## Imported by") && markdown.contains("src/c.rs"));
    assert_eq!(page["digest"].as_str().unwrap().len(), 16);
    assert_eq!(page["next"][0]["arguments"]["action"], "children");
    assert!(output["snapshot"].is_string());
}

#[tokio::test]
async fn targets_can_be_written_as_a_path_a_folder_or_nothing() {
    let (_fixture, tools) = ready().await;
    let level = |target: &str| {
        let tools = tools.clone();
        let target = target.to_owned();
        async move {
            call(&tools, "repo_wiki", json!({"target": target})).await.0["result"]["level"].clone()
        }
    };
    assert_eq!(level("src/a.rs").await, "file");
    assert_eq!(level("src").await, "folder");
    assert_eq!(level("src/").await, "folder");
    assert_eq!(level("dir:src/tools").await, "folder");
    assert_eq!(level("").await, "repository");
    assert_eq!(level("repo:").await, "repository");
}

#[tokio::test]
async fn children_walk_the_tree_and_json_gives_the_structured_page() {
    let (_fixture, tools) = ready().await;
    let (top, _) = call(
        &tools,
        "repo_wiki",
        json!({"action": "children", "target": ""}),
    )
    .await;
    assert!(result(&top)["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"].as_str().unwrap().starts_with("package:")));
    let (json_page, _) = call(
        &tools,
        "repo_wiki",
        json!({"target": "src/a.rs", "format": "json"}),
    )
    .await;
    let page = &result(&json_page)["page"];
    assert_eq!(page["id"], "file:src/a.rs");
    assert!(page["sections"][0]["facts"][0]["citations"][0]["content_hash"].is_string());
}

#[tokio::test]
async fn pages_that_cannot_be_written_have_a_status_and_bad_requests_are_errors() {
    let (_fixture, tools) = ready().await;
    let (rb, is_error) = call(&tools, "repo_wiki", json!({"target": "scripts/tool.rb"})).await;
    assert!(!is_error);
    assert_eq!(result(&rb)["status"], "unsupported");
    let (missing, _) = call(&tools, "repo_wiki", json!({"target": "src/nothing.rs"})).await;
    assert_eq!(result(&missing)["status"], "not_found");
    for (arguments, code) in [
        (json!({"target": "x", "surprise": 1}), "invalid_request"),
        (json!({"target": "bogus:thing"}), "not_found_or_invalid"),
    ] {
        let (output, is_error) = call(&tools, "repo_wiki", arguments.clone()).await;
        if code == "invalid_request" {
            assert!(is_error, "{arguments}");
            assert_eq!(error_code(&output), code);
        } else {
            // An unknown prefix is read as a path, so it is a page with a status, not a failure.
            assert!(!is_error, "{output}");
        }
    }
}

#[tokio::test]
async fn it_needs_the_graph_toolset_and_a_snapshot() {
    let fixture = fixture();
    let git_only = harness_tool_repository_intelligence::RepositoryIntelligence::new()
        .with_git(fixture.git.clone());
    assert!(git_only.executor_for("repo_wiki").is_none());
    assert!(harness_tool_repository_intelligence::is_repository_tool(
        "repo_wiki"
    ));
    let (output, is_error) = call(
        &fixture.graph_only(),
        "repo_wiki",
        json!({"target": "src/a.rs"}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&output), "snapshot_not_found");
}

#[tokio::test]
async fn it_agrees_with_repo_context_about_the_same_file() {
    let (_fixture, tools) = ready().await;
    let (context, _) = call(&tools, "repo_context", json!({"target": "src/a.rs"})).await;
    let (wiki, _) = call(
        &tools,
        "repo_wiki",
        json!({"target": "src/a.rs", "format": "json"}),
    )
    .await;
    let importers = &result(&context)["sections"]["dependents"];
    let from_context: Vec<&str> = importers
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["file"].as_str().unwrap())
        .collect();
    let page = &result(&wiki)["page"];
    let section = page["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "imported_by")
        .unwrap();
    let from_wiki: Vec<String> = section["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            f["refs"][0]
                .as_str()
                .unwrap()
                .trim_start_matches("file:")
                .to_owned()
        })
        .collect();
    assert_eq!(from_context, from_wiki);
}

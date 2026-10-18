mod support;

use serde_json::{json, Value};
use support::*;
use tokio_util::sync::CancellationToken;

/// The standard fixture plus files that exercise every status.
fn repository() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    write_repository(dir.path());
    write(
        dir.path(),
        "web/src/x.test.ts",
        "import { x } from './x';\nexport const t = x;\n",
    );
    write(dir.path(), "scripts/tool.rb", "puts 'hi'\n");
    write(
        dir.path(),
        "node_modules/dep/index.js",
        "module.exports = 1;\n",
    );
    write_history(dir.path());
    let (graph, git) = services(dir.path());
    Fixture { dir, graph, git }
}

async fn context(
    tools: &harness_tool_repository_intelligence::RepositoryIntelligence,
    arguments: Value,
) -> (Value, bool) {
    call(tools, "repo_context", arguments).await
}

async fn ready() -> (
    Fixture,
    harness_tool_repository_intelligence::RepositoryIntelligence,
) {
    let fixture = repository();
    let tools = fixture.everything();
    call(&tools, "repo_graph.refresh", json!({})).await;
    call(&tools, "repo_git.refresh", json!({})).await;
    call(&tools, "repo_findings.analyze", json!({})).await;
    (fixture, tools)
}

fn status(output: &Value) -> &str {
    output["result"]["status"].as_str().unwrap_or("<none>")
}

fn names<'a>(items: &'a Value, key: &str) -> Vec<&'a str> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i[key].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn connections_of_a_file_lists_imports_dependents_and_a_one_line_summary() {
    let (_fixture, tools) = ready().await;
    let (output, is_error) = context(&tools, json!({"target": "src/a.rs"})).await;
    assert!(!is_error, "{output}");
    let result = &output["result"];
    assert_eq!(status(&output), "ok", "{result}");
    assert_eq!(result["target"]["language"], "rust");
    assert_eq!(result["target"]["package"], "app");
    assert_eq!(
        names(&result["sections"]["dependents"], "file"),
        ["src/b.rs", "src/c.rs", "src/lib.rs"]
    );
    assert_eq!(names(&result["sections"]["imports"], "name"), ["src/b.rs"]);
    assert!(result["summary"]
        .as_str()
        .unwrap()
        .contains("imported by 3 files"));
    assert!(result["sections"]["history"]["commits"].as_u64().unwrap() >= 4);
    assert_eq!(
        result["sections"]["history"]["co_changed_with"][0]["file"],
        "src/b.rs"
    );
    assert_eq!(result["support"]["imports"], "resolved");
    assert!(output["git_snapshot"].is_string());
    let follow = result["next"].as_array().unwrap();
    assert!(follow
        .iter()
        .any(|n| n["tool"] == "repo_context" && n["arguments"]["mode"] == "blast_radius"));
}

#[tokio::test]
async fn a_language_that_cannot_be_read_is_an_answer_not_an_error() {
    let (_fixture, tools) = ready().await;
    let (output, is_error) = context(&tools, json!({"target": "scripts/tool.rb"})).await;
    assert!(!is_error, "{output}");
    assert_eq!(status(&output), "unsupported");
    let result = &output["result"];
    assert_eq!(result["target"]["language"], "Ruby");
    let supported = result["support"]["supported_languages"].as_array().unwrap();
    assert!(supported.iter().any(|l| l == "rust") && supported.iter().any(|l| l == "typescript"));
    assert!(result["summary"]
        .as_str()
        .unwrap()
        .contains("Ruby is not supported"));
    assert!(result["sections"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn every_other_status_is_reported_for_the_path_that_causes_it() {
    let (fixture, tools) = ready().await;
    let (missing, _) = context(&tools, json!({"target": "src/nothing.rs"})).await;
    assert_eq!(status(&missing), "not_found");
    let (ignored, _) = context(&tools, json!({"target": "node_modules/dep/index.js"})).await;
    assert_eq!(status(&ignored), "not_indexed");
    assert!(ignored["result"]["summary"]
        .as_str()
        .unwrap()
        .contains("node_modules"));

    write(
        fixture.dir.path(),
        "src/a.rs",
        "use crate::b::B;\npub struct A;\n// edited\n",
    );
    let (changed, _) = context(&tools, json!({"target": "src/a.rs"})).await;
    assert_eq!(status(&changed), "stale");
    assert!(changed["result"]["next"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["tool"] == "repo_graph.refresh"));
    // The last known answer is still there.
    assert_eq!(
        changed["result"]["sections"]["dependents"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    write(fixture.dir.path(), "src/new.rs", "pub struct N;\n");
    let (added, _) = context(&tools, json!({"target": "src/new.rs"})).await;
    assert_eq!(status(&added), "stale");
    assert!(added["result"]["summary"]
        .as_str()
        .unwrap()
        .contains("added after"));
}

#[tokio::test]
async fn blast_radius_separates_tests_dependents_and_co_change_hints() {
    let (_fixture, tools) = ready().await;
    let (output, is_error) = context(
        &tools,
        json!({"target": "web/src/x.ts", "mode": "blast_radius"}),
    )
    .await;
    assert!(!is_error, "{output}");
    let sections = &output["result"]["sections"];
    assert_eq!(names(&sections["tests"], "file"), ["web/src/x.test.ts"]);
    assert_eq!(names(&sections["dependents"], "file"), ["web/src/index.ts"]);
    assert_eq!(sections["affected"]["files"], 2);
    assert!(sections["co_change_hints"]["label"]
        .as_str()
        .unwrap()
        .contains("not a dependency"));
    assert!(output["component_versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "rule:test-convention"));

    let (cycle, _) = context(
        &tools,
        json!({"target": "src/a.rs", "mode": "blast_radius", "depth": 1}),
    )
    .await;
    assert_eq!(cycle["result"]["sections"]["affected"]["by_depth"]["1"], 3);
}

#[tokio::test]
async fn orient_shows_one_level_with_loose_files_as_one_entry() {
    let (_fixture, tools) = ready().await;
    let (root, is_error) = context(&tools, json!({"target": "", "mode": "orient"})).await;
    assert!(!is_error, "{root}");
    let sections = &root["result"]["sections"];
    let folders = names(&sections["folders"], "label");
    assert!(
        folders.contains(&"src") && folders.contains(&"web"),
        "{folders:?}"
    );
    assert!(!folders.contains(&"node_modules"));
    assert_eq!(sections["languages"]["rust"], 4);
    let (web, _) = context(&tools, json!({"target": "dir:web", "mode": "orient"})).await;
    assert_eq!(web["result"]["target"]["leaf"], false);
    assert_eq!(
        names(&web["result"]["sections"]["folders"], "label"),
        ["src"]
    );
    // A folder holding only files is a leaf: its files are listed.
    let (leaf, _) = context(&tools, json!({"target": "dir:web/src", "mode": "orient"})).await;
    assert_eq!(leaf["result"]["target"]["leaf"], true);
    assert_eq!(names(&leaf["result"]["sections"]["files"], "file").len(), 3);
    let (most, _) = context(&tools, json!({"target": "src", "mode": "orient"})).await;
    assert_eq!(
        most["result"]["sections"]["most_depended_on"][0]["file"],
        "src/a.rs"
    );
}

#[tokio::test]
async fn changes_report_touched_packages_and_what_could_not_be_read() {
    let (_fixture, tools) = ready().await;
    let (output, is_error) = context(&tools, json!({"mode": "changes", "targets": ["web/src/x.ts", "scripts/tool.rb", "src/nothing.rs"]})).await;
    assert!(!is_error, "{output}");
    assert_eq!(status(&output), "partial");
    let sections = &output["result"]["sections"];
    assert_eq!(names(&sections["changed"], "file"), ["web/src/x.ts"]);
    assert_eq!(names(&sections["tests"], "file"), ["web/src/x.test.ts"]);
    let skipped = sections["skipped_targets"].as_array().unwrap();
    assert_eq!(
        skipped
            .iter()
            .map(|s| s["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["unsupported", "not_found"]
    );
    let (cycle, _) = context(&tools, json!({"mode": "changes", "targets": ["src/a.rs"]})).await;
    assert_eq!(
        cycle["result"]["sections"]["cycles"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(status(&cycle), "ok");
}

#[tokio::test]
async fn the_budget_trims_deterministically_and_says_what_it_left_out() {
    let fixture = repository();
    for i in 0..80 {
        write(
            fixture.dir.path(),
            &format!("web/src/gen/f{i:02}.ts"),
            "import { x } from '../x';\nexport const v = x;\n",
        );
    }
    let tools = fixture.graph_only();
    call(&tools, "repo_graph.refresh", json!({})).await;
    let request = json!({"target": "web/src/x.ts", "budget": 1000});
    let (first, _) = context(&tools, request.clone()).await;
    let (second, _) = context(&tools, request).await;
    assert_eq!(first, second);
    let result = &first["result"];
    let kept = result["sections"]["dependents"].as_array().unwrap().len();
    assert!(kept < 82, "{kept}");
    assert_eq!(result["truncated"]["reason"], "budget");
    assert_eq!(
        result["truncated"]["omitted"]["dependents"]
            .as_u64()
            .unwrap() as usize
            + kept,
        82
    );
    assert_eq!(first["truncation"], "budget");
    assert!(result["next"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["tool"] == "repo_graph.dependents"));
    let (large, _) = context(&tools, json!({"target": "web/src/x.ts", "budget": 20000})).await;
    assert_eq!(
        large["result"]["sections"]["dependents"]
            .as_array()
            .unwrap()
            .len(),
        82
    );
    assert!(large["result"]["truncated"].is_null());
}

#[tokio::test]
async fn sections_a_caller_asks_for_but_cannot_have_are_reported_as_missing() {
    let fixture = repository();
    let tools = fixture.graph_only();
    call(&tools, "repo_graph.refresh", json!({})).await;
    let (quiet, _) = context(&tools, json!({"target": "src/a.rs"})).await;
    assert_eq!(
        status(&quiet),
        "ok",
        "an unavailable optional toolset is not a gap unless asked for"
    );
    assert_eq!(quiet["result"]["support"]["history"], "unavailable");
    let (asked, _) = context(
        &tools,
        json!({"target": "src/a.rs", "include": ["history", "findings"]}),
    )
    .await;
    assert_eq!(status(&asked), "partial");
    let missing = asked["result"]["support"]["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 2);
    assert!(asked["result"]["sections"].get("history").is_none());
}

#[tokio::test]
async fn bad_requests_foreign_snapshots_and_cancellation_are_errors() {
    let (_fixture, tools) = ready().await;
    for (arguments, code) in [
        (json!({"target": "a", "surprise": 1}), "invalid_request"),
        (json!({"target": "../outside"}), "invalid_request"),
        (
            json!({"target": "src/a.rs", "targets": ["x"]}),
            "invalid_request",
        ),
        (json!({"mode": "changes"}), "invalid_request"),
        (json!({"target": "src/a.rs", "depth": 9}), "limit_exceeded"),
        (
            json!({"target": "src/a.rs", "budget": 10}),
            "limit_exceeded",
        ),
        (
            json!({"target": "src/a.rs", "snapshot": "nope"}),
            "snapshot_not_found",
        ),
    ] {
        let (output, is_error) = context(&tools, arguments.clone()).await;
        assert!(is_error, "{arguments}: {output}");
        assert_eq!(error_code(&output), code, "{arguments}");
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (output, is_error) = call_with(
        &tools,
        "repo_context",
        json!({"target": "src/a.rs"}),
        cancel,
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&output), "cancelled");
}

#[tokio::test]
async fn it_needs_the_graph_toolset_and_follows_the_registry() {
    let fixture = repository();
    let git_only = harness_tool_repository_intelligence::RepositoryIntelligence::new()
        .with_git(fixture.git.clone());
    assert!(git_only.executor_for("repo_context").is_none());
    assert!(harness_tool_repository_intelligence::is_repository_tool(
        "repo_context"
    ));
    // What the tool calls supported is what the graph's adapters read.
    let path =
        |p: &str| harness_workspace::repository_intelligence::RootRelativePath::new(p).unwrap();
    assert!(fixture.graph.classify(&path("src/a.rs")).is_some());
    assert!(fixture.graph.classify(&path("web/src/x.ts")).is_some());
    assert!(fixture.graph.classify(&path("scripts/tool.rb")).is_none());
}

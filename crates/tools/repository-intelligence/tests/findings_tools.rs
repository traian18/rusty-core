mod support;

use harness_tool_repository_intelligence::RepositoryIntelligence;
use serde_json::json;
use support::*;

async fn ready() -> (Fixture, RepositoryIntelligence) {
    let fixture = fixture();
    // Untracked and unreferenced: a candidate for `unreferenced-file`.
    write(
        fixture.dir.path(),
        "web/src/dead.ts",
        "export const dead = 1;\n",
    );
    let tools = fixture.everything();
    call(&tools, "repo_graph.refresh", json!({})).await;
    call(&tools, "repo_git.refresh", json!({})).await;
    (fixture, tools)
}

#[tokio::test]
async fn analyze_summary_list_get_and_explain_form_a_complete_workflow() {
    let (_fixture, tools) = ready().await;
    let (analyzed, is_error) = call(&tools, "repo_findings.analyze", json!({})).await;
    assert!(!is_error, "{analyzed}");
    assert_eq!(analyzed["schema_version"], 1);
    assert!(analyzed["snapshot"].is_string() && analyzed["git_snapshot"].is_string());
    let set = analyzed["result"]["finding_set"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(analyzed["component_versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "finding-rule:import-cycle"));
    assert!(
        analyzed["result"]["summary"]["by_rule"]["import-cycle"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let (summary, _) = call(&tools, "repo_findings.summary", json!({})).await;
    assert_eq!(summary["result"]["finding_set"], set.as_str());

    let (cycles, is_error) = call(
        &tools,
        "repo_findings.list",
        json!({"rules": ["import-cycle"], "min_severity": "medium"}),
    )
    .await;
    assert!(!is_error, "{cycles}");
    let entries = cycles["result"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let finding = &entries[0]["finding"];
    assert_eq!(finding["id"], "import-cycle:src/a.rs");
    assert_eq!(finding["kind"], "measurable");
    assert_eq!(finding["locations"][0]["line"], 1);

    let (got, is_error) = call(
        &tools,
        "repo_findings.get",
        json!({"finding": "import-cycle:src/a.rs"}),
    )
    .await;
    assert!(!is_error);
    assert!(got["result"]["finding"]["rationale"]
        .as_str()
        .unwrap()
        .contains("loaded, tested and changed together"));

    let (explained, is_error) = call(
        &tools,
        "repo_findings.explain",
        json!({"finding_set": set, "finding": "import-cycle:src/a.rs"}),
    )
    .await;
    assert!(!is_error, "{explained}");
    assert_eq!(
        explained["result"]["import_sites"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(explained["result"]["repository_snapshot_available"], true);
    assert!(explained["result"]["rule_description"]
        .as_str()
        .unwrap()
        .contains("mutually reachable"));
}

#[tokio::test]
async fn candidates_carry_the_caveat_and_filters_and_paging_work() {
    let (_fixture, tools) = ready().await;
    call(&tools, "repo_findings.analyze", json!({})).await;
    let (candidates, _) = call(
        &tools,
        "repo_findings.list",
        json!({"kind": "candidate", "limit": 1}),
    )
    .await;
    let first = &candidates["result"]["entries"][0]["finding"];
    assert_eq!(first["kind"], "candidate");
    assert!(first["interpretation"]
        .as_str()
        .unwrap()
        .contains("not evidence"));

    let mut seen = 0;
    let mut cursor = None;
    loop {
        let arguments = match &cursor {
            Some(cursor) => json!({"limit": 2, "cursor": cursor}),
            None => json!({"limit": 2}),
        };
        let (page, is_error) = call(&tools, "repo_findings.list", arguments).await;
        assert!(!is_error);
        seen += page["result"]["entries"].as_array().unwrap().len();
        match page["result"]["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    let (summary, _) = call(&tools, "repo_findings.summary", json!({})).await;
    assert_eq!(
        seen as u64,
        summary["result"]["summary"]["total"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn errors_are_structured_and_the_toolset_is_opt_in() {
    let (fixture, tools) = ready().await;
    let (none, is_error) = call(&tools, "repo_findings.list", json!({})).await;
    assert!(is_error);
    assert_eq!(
        error_code(&none),
        "snapshot_not_found",
        "nothing analysed yet"
    );
    call(&tools, "repo_findings.analyze", json!({})).await;
    for (tool, arguments, code) in [
        (
            "repo_findings.get",
            json!({"finding": "nope:none"}),
            "finding_not_found",
        ),
        ("repo_findings.get", json!({}), "invalid_request"),
        ("repo_findings.list", json!({"limit": 0}), "limit_exceeded"),
        (
            "repo_findings.list",
            json!({"cursor": "garbage"}),
            "invalid_cursor",
        ),
        (
            "repo_findings.list",
            json!({"finding_set": "gone"}),
            "snapshot_not_found",
        ),
        (
            "repo_findings.list",
            json!({"min_severity": "catastrophic"}),
            "invalid_request",
        ),
        (
            "repo_findings.list",
            json!({"file": "/etc/passwd"}),
            "invalid_request",
        ),
        (
            "repo_findings.analyze",
            json!({"snapshot": "missing"}),
            "snapshot_not_found",
        ),
        (
            "repo_findings.analyze",
            json!({"unexpected": true}),
            "invalid_request",
        ),
        (
            "repo_findings.explain",
            json!({"finding": "nope:none"}),
            "finding_not_found",
        ),
    ] {
        let (output, is_error) = call(&tools, tool, arguments.clone()).await;
        assert!(is_error, "{tool} {arguments}: {output}");
        assert_eq!(error_code(&output), code, "{tool} {arguments}: {output}");
    }

    let cancel = harness_tools::CancellationToken::new();
    cancel.cancel();
    let (cancelled, is_error) = call_with(&tools, "repo_findings.analyze", json!({}), cancel).await;
    assert!(is_error);
    assert_eq!(error_code(&cancelled), "cancelled");

    // The three toolsets are independent opt-ins.
    assert!(fixture.all().executor_for("repo_findings.list").is_none());
    assert_eq!(tools.executors().len(), 21);
    let only = RepositoryIntelligence::new().with_findings(fixture.findings());
    assert_eq!(only.executors().len(), 5);
    assert!(only.executor_for("repo_graph.refresh").is_none());
    assert!(harness_tool_repository_intelligence::is_repository_tool(
        "repo_findings.get"
    ));
    assert!(only.executor_for("repo_findings.nonexistent").is_none());
}

#[tokio::test]
async fn without_a_git_snapshot_the_set_says_which_rules_were_skipped() {
    let fixture = fixture();
    let tools = fixture.everything();
    call(&tools, "repo_graph.refresh", json!({})).await;
    // No repo_git.refresh: Git-based rules have no input.
    let (analyzed, is_error) = call(&tools, "repo_findings.analyze", json!({})).await;
    assert!(!is_error, "{analyzed}");
    assert_eq!(analyzed["partial"], true);
    let skipped = analyzed["result"]["summary"]["skipped_rules"]
        .as_array()
        .unwrap();
    assert!(skipped
        .iter()
        .any(|s| s["rule"]["id"] == "implicit-coupling"));
    let (opt_out, _) = call(&tools, "repo_findings.analyze", json!({"use_git": false})).await;
    assert!(opt_out["git_snapshot"].is_null());
}

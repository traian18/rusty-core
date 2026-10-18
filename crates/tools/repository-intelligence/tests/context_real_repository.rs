//! Measures `repo_context` on a real checkout. Run by hand:
//! `MEASURE_ROOT=<repo> cargo test -p harness-tool-repository-intelligence --test context_real_repository -- --ignored --nocapture`
mod support;

use std::time::Instant;

use serde_json::{json, Value};
use support::*;

async fn timed(
    tools: &harness_tool_repository_intelligence::RepositoryIntelligence,
    label: &str,
    arguments: Value,
) -> Value {
    let started = Instant::now();
    let (output, is_error) = call(tools, "repo_context", arguments).await;
    let size = output.to_string().len();
    println!(
        "{label}: {:?}, {size} chars, status {}",
        started.elapsed(),
        output["result"]["status"]
    );
    assert!(!is_error, "{label}: {output}");
    assert!(size < 21_000, "{label} exceeded the budget: {size}");
    output
}

#[tokio::test]
#[ignore = "measures a real repository; run by hand"]
async fn measure_every_mode() {
    let root = std::path::PathBuf::from(std::env::var("MEASURE_ROOT").expect("set MEASURE_ROOT"));
    let (graph, git) = services(&root);
    let tools = harness_tool_repository_intelligence::RepositoryIntelligence::new()
        .with_graph(graph.clone())
        .with_git(git);
    let started = Instant::now();
    call(&tools, "repo_graph.refresh", json!({})).await;
    println!("refresh: {:?}", started.elapsed());

    timed(
        &tools,
        "orient root",
        json!({"mode": "orient", "target": ""}),
    )
    .await;
    let snapshot = graph.current().unwrap();
    let hub = snapshot
        .imports
        .files
        .iter()
        .max_by_key(|file| {
            snapshot
                .imports
                .edges
                .iter()
                .filter(|e| e.resolution.target_file() == Some(&file.path))
                .count()
        })
        .unwrap()
        .path
        .as_path()
        .display()
        .to_string();
    println!("most imported file: {hub}");
    timed(&tools, "connections (hub)", json!({"target": hub})).await;
    timed(
        &tools,
        "blast_radius (hub)",
        json!({"target": hub, "mode": "blast_radius", "depth": 3}),
    )
    .await;
    let folder = std::path::Path::new(&hub)
        .parent()
        .unwrap()
        .display()
        .to_string();
    timed(
        &tools,
        "blast_radius (folder)",
        json!({"target": folder, "mode": "blast_radius"}),
    )
    .await;
    timed(
        &tools,
        "orient (folder)",
        json!({"target": folder, "mode": "orient"}),
    )
    .await;
    timed(
        &tools,
        "changes",
        json!({"mode": "changes", "targets": [hub]}),
    )
    .await;
}

#[tokio::test]
#[ignore = "measures a real repository; run by hand"]
async fn show_where_the_characters_go() {
    let root = std::path::PathBuf::from(std::env::var("MEASURE_ROOT").expect("set MEASURE_ROOT"));
    let (graph, git) = services(&root);
    let tools = harness_tool_repository_intelligence::RepositoryIntelligence::new()
        .with_graph(graph)
        .with_git(git);
    call(&tools, "repo_graph.refresh", json!({})).await;
    let output = timed(
        &tools,
        "blast",
        json!({"target": std::env::var("HUB").unwrap(), "mode": "blast_radius", "depth": 3}),
    )
    .await;
    let result = &output["result"];
    for key in ["target", "support", "summary", "truncated", "next"] {
        println!("{key}: {}", result[key].to_string().len());
    }
    for (key, value) in result["sections"].as_object().unwrap() {
        println!("section {key}: {}", value.to_string().len());
    }
    println!(
        "components: {}",
        output["component_versions"].to_string().len()
    );
}

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use harness_tool_repository_intelligence::RepositoryIntelligence;
use harness_workspace::{
    import_graph::{ImportPage, ImportQuery, ImportQueryError},
    repository_intelligence::{
        DiscoveryCapabilities, DiscoveryError, RepositoryGraph, RepositorySnapshot, SnapshotId,
    },
};
use serde_json::json;
use support::*;
use tokio_util::sync::CancellationToken;

/// Two contexts over the same root share nothing: snapshots published through one are not
/// visible through the other until the other builds them itself.
#[tokio::test]
async fn contexts_over_the_same_root_do_not_share_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    write_repository(dir.path());
    write_history(dir.path());
    let (graph_a, git_a) = services(dir.path());
    let (graph_b, git_b) = services(dir.path());
    let a = RepositoryIntelligence::new()
        .with_graph(graph_a)
        .with_git(git_a);
    let b = RepositoryIntelligence::new()
        .with_graph(graph_b)
        .with_git(git_b);

    let (refreshed, _) = call(&a, "repo_graph.refresh", json!({})).await;
    let id = refreshed["snapshot"].as_str().unwrap().to_owned();
    let (git_refreshed, _) = call(&a, "repo_git.refresh", json!({})).await;
    let git_id = git_refreshed["git_snapshot"].as_str().unwrap().to_owned();

    let (graph_miss, is_error) = call(
        &b,
        "repo_graph.dependencies",
        json!({"file": "src/a.rs", "snapshot": id}),
    )
    .await;
    assert!(is_error);
    assert_eq!(error_code(&graph_miss), "snapshot_not_found");
    let (current_miss, _) = call(&b, "repo_graph.dependencies", json!({"file": "src/a.rs"})).await;
    assert_eq!(error_code(&current_miss), "snapshot_not_found");
    let (git_miss, _) = call(&b, "repo_git.churn", json!({"git_snapshot": git_id})).await;
    assert_eq!(error_code(&git_miss), "snapshot_not_found");

    // The first context is unaffected.
    let (still, is_error) = call(&a, "repo_graph.dependencies", json!({"file": "src/a.rs"})).await;
    assert!(!is_error, "{still}");
}

/// A graph that fails like a broken dependency, to prove failures surface as structured errors.
struct BrokenGraph;

#[async_trait]
impl RepositoryGraph for BrokenGraph {
    async fn refresh(&self, _: CancellationToken) -> Result<RepositorySnapshot, DiscoveryError> {
        Err(DiscoveryError::Failed("disk unavailable".into()))
    }
    fn capabilities(&self) -> DiscoveryCapabilities {
        DiscoveryCapabilities::default()
    }
    fn current(&self) -> Option<Arc<RepositorySnapshot>> {
        None
    }
    fn get(&self, id: &SnapshotId) -> Result<Arc<RepositorySnapshot>, DiscoveryError> {
        Err(DiscoveryError::SnapshotNotFound(id.0.clone()))
    }
    fn dependencies(&self, _: &ImportQuery) -> Result<ImportPage, ImportQueryError> {
        Err(ImportQueryError::SnapshotNotFound("x".into()))
    }
    fn dependents(&self, _: &ImportQuery) -> Result<ImportPage, ImportQueryError> {
        Err(ImportQueryError::SnapshotNotFound("x".into()))
    }
}

#[tokio::test]
async fn tools_run_against_any_repository_graph_and_surface_dependency_failures() {
    let tools = RepositoryIntelligence::new().with_graph(Arc::new(BrokenGraph));
    let (output, is_error) = call(&tools, "repo_graph.refresh", json!({})).await;
    assert!(is_error);
    assert_eq!(error_code(&output), "dependency_failure");
    assert!(output["error"]["message"]
        .as_str()
        .unwrap()
        .contains("disk unavailable"));

    let (missing, _) = call(&tools, "repo_graph.dependents", json!({"file": "a.rs"})).await;
    assert_eq!(error_code(&missing), "snapshot_not_found");
    // Group queries default to `not_configured` on graphs that do not implement them.
    let (caps, is_error) = call(&tools, "repo_graph.capabilities", json!({})).await;
    assert!(!is_error);
    assert!(caps["result"]["groups"].is_null());
}

#[tokio::test]
async fn partial_coverage_is_reported_in_the_envelope() {
    let dir = tempfile::tempdir().unwrap();
    write_repository(dir.path());
    write(
        dir.path(),
        "src/big.rs",
        &format!("{}\n", "// padding ".repeat(2000)),
    );
    let graph = harness_workspace::repository_intelligence::RepositoryGraphBuilder::new(
        std::fs::canonicalize(dir.path()).unwrap(),
    )
    .unwrap()
    .import_limits(harness_workspace::import_graph::ImportLimits {
        max_file_bytes: 1024,
        ..Default::default()
    })
    .import_support(harness_workspace::import_graph::ImportSupport {
        adapter: Arc::new(harness_repository_imports::RustImportAdapter),
        resolver: Arc::new(harness_repository_imports::RustImportResolver),
    })
    .unwrap()
    .build();
    let tools = RepositoryIntelligence::new().with_graph(Arc::new(graph));
    let (refreshed, is_error) = call(&tools, "repo_graph.refresh", json!({})).await;
    assert!(!is_error, "{refreshed}");
    assert_eq!(refreshed["partial"], true);
    let (edges, _) = call(
        &tools,
        "repo_graph.dependencies",
        json!({"file": "src/lib.rs"}),
    )
    .await;
    assert_eq!(edges["partial"], true);
}

//! A session built with explicitly authorized repository intelligence can run `repo_graph.*` and
//! `repo_git.*` tools against it, and the shared instances observe the effect. Sessions built
//! without it, or with only one of the two, keep their existing tools and fail closed for the rest.

use std::collections::HashMap;
use std::sync::Arc;

use harness_engine::SessionBuilder;
use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{RequestId, ToolCallId, ToolId};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy,
};
use harness_protocol::usage::{Cost, ModelUsage};
use harness_repository_imports::{RustImportAdapter, RustImportResolver};
use harness_runtime::testing::FakeBackend;
use harness_runtime::traits::Workspace;
use harness_tool_repository_intelligence::RepositoryIntelligence;
use harness_workspace::FsWorkspace;
use harness_workspace::{
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn toolset(names: &[&str]) -> AgentToolset {
    let mut tools = HashMap::new();
    for name in names {
        let id = ToolId::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: (*name).to_owned(),
                    description: "test".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            },
        );
    }
    AgentToolset { tools }
}

fn backend(tool: &str, arguments: serde_json::Value) -> Arc<FakeBackend> {
    let request_id = RequestId::new();
    let result = ExecutionResult {
        request_id,
        usage: ModelUsage::default(),
        cost: Cost::default(),
        finish_reason: "tool_use".into(),
    };
    Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::ToolCallRequested {
                    request_id,
                    call: ToolCall {
                        id: ToolCallId::new(),
                        name: tool.to_owned(),
                        arguments,
                    },
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: result.clone(),
                },
            ])
            .with_result(result),
    )
}

async fn wait_for_tool_completion(
    session: &harness_engine::SessionHandle,
    subscriber: &mut tokio::sync::broadcast::Receiver<harness_protocol::events::AgentEventEnvelope>,
) -> Vec<AgentEvent> {
    session.send("go").await.expect("send");
    let mut events = Vec::new();
    for _ in 0..50 {
        while let Ok(envelope) = subscriber.try_recv() {
            events.push(envelope.event);
        }
        if events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    events
}

fn graph_for(root: &std::path::Path) -> Arc<dyn RepositoryGraph> {
    Arc::new(
        RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
            .unwrap()
            .import_support(ImportSupport {
                adapter: Arc::new(RustImportAdapter),
                resolver: Arc::new(RustImportResolver),
            })
            .unwrap()
            .build(),
    )
}

fn repository() -> (tempfile::TempDir, Arc<dyn Workspace>) {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "pub mod a;\n").unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "pub struct A;\n").unwrap();
    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    (dir, workspace)
}

async fn run_tool(
    intelligence: Option<RepositoryIntelligence>,
    workspace: Arc<dyn Workspace>,
    tool: &str,
) -> harness_protocol::tools::ToolResultSummary {
    let names = toolset(&[tool]);
    let builder = match intelligence {
        Some(intelligence) => SessionBuilder::new().toolset_with_repository_intelligence(
            names,
            workspace,
            intelligence,
        ),
        None => SessionBuilder::new().toolset(names, workspace),
    };
    let session = builder
        .backend(backend(tool, serde_json::json!({})))
        .start()
        .await
        .expect("session builds");
    let mut subscriber = session.subscribe();
    wait_for_tool_completion(&session, &mut subscriber)
        .await
        .into_iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallCompleted { result, .. } => Some(result),
            _ => None,
        })
        .expect("the call is answered")
}

#[tokio::test]
async fn session_with_authorized_intelligence_refreshes_the_shared_graph() {
    let (dir, workspace) = repository();
    let graph = graph_for(dir.path());
    let intelligence = RepositoryIntelligence::new().with_graph(graph.clone());
    let summary = run_tool(Some(intelligence), workspace, "repo_graph.refresh").await;
    assert!(!summary.has_error, "{summary:?}");
    // The graph instance handed to the session now holds the snapshot the tool published.
    assert_eq!(
        graph
            .current()
            .expect("published through the tool")
            .graph
            .packages
            .len(),
        1
    );
}

#[tokio::test]
async fn sessions_without_repository_intelligence_fail_closed() {
    let (_dir, workspace) = repository();
    for tool in ["repo_graph.refresh", "repo_git.refresh"] {
        let summary = run_tool(None, workspace.clone(), tool).await;
        assert!(
            summary.has_error,
            "{tool} must fail without authorized intelligence: {summary:?}"
        );
    }
}

#[tokio::test]
async fn the_git_toolset_needs_its_own_opt_in() {
    let (dir, workspace) = repository();
    let graph_only = RepositoryIntelligence::new().with_graph(graph_for(dir.path()));
    let summary = run_tool(
        Some(graph_only.clone()),
        workspace.clone(),
        "repo_git.refresh",
    )
    .await;
    assert!(
        summary.has_error,
        "repo_git.* is not enabled by a graph alone: {summary:?}"
    );
    let ok = run_tool(Some(graph_only), workspace, "repo_graph.capabilities").await;
    assert!(!ok.has_error, "{ok:?}");
}

#[tokio::test]
async fn the_findings_toolset_runs_in_a_session_over_the_shared_graph() {
    let (dir, workspace) = repository();
    let graph = graph_for(dir.path());
    graph
        .refresh(CancellationToken::new())
        .await
        .expect("snapshot");
    let findings: Arc<dyn harness_findings::FindingsAnalysis> =
        Arc::new(harness_findings::FindingsService::new(
            graph.clone(),
            None,
            Arc::new(
                harness_findings::DefaultFindingsAnalyzer::standard(
                    harness_findings::FindingsConfig::default(),
                )
                .expect("config"),
            ),
            Arc::new(harness_findings::FsSourceTextReader::new(dir.path())),
        ));
    let intelligence = RepositoryIntelligence::new()
        .with_graph(graph)
        .with_findings(findings.clone());
    let summary = run_tool(
        Some(intelligence),
        workspace.clone(),
        "repo_findings.analyze",
    )
    .await;
    assert!(!summary.has_error, "{summary:?}");
    assert!(
        findings.current().is_some(),
        "the shared findings service observed the tool call"
    );

    // Findings are a separate opt-in: a graph alone does not enable them.
    let graph_only = RepositoryIntelligence::new().with_graph(graph_for(dir.path()));
    let summary = run_tool(Some(graph_only), workspace, "repo_findings.analyze").await;
    assert!(summary.has_error, "{summary:?}");
}

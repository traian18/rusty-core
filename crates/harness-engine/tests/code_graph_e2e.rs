//! A session built with an explicitly authorized call graph can run `code_graph.*` tools against
//! it, and the same shared graph instance observes the effect. Sessions built without a graph keep
//! their existing tools and do not expose graph behaviour.

use std::collections::HashMap;
use std::sync::Arc;

use harness_code_analysis::{CallGraph, DefaultCallGraph, FindCallablesRequest};
use harness_engine::SessionBuilder;
use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
use harness_protocol::events::AgentEvent;
use harness_protocol::ids::{RequestId, ToolCallId, ToolId};
use harness_protocol::tools::{
    AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy,
};
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::testing::FakeBackend;
use harness_runtime::traits::Workspace;
use harness_workspace::FsWorkspace;
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

#[tokio::test]
async fn session_with_an_authorized_graph_refreshes_the_shared_instance() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.py"),
        "def a():\n    pass\ndef b():\n    a()\n",
    )
    .unwrap();
    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    let graph = Arc::new(DefaultCallGraph::new(dir.path()));
    let shared: Arc<dyn CallGraph> = graph.clone();

    let session = SessionBuilder::new()
        .toolset_with_call_graph(toolset(&["code_graph.refresh"]), workspace, shared)
        .backend(backend("code_graph.refresh", serde_json::json!({})))
        .start()
        .await
        .expect("session builds");
    let mut subscriber = session.subscribe();
    let events = wait_for_tool_completion(&session, &mut subscriber).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })),
        "tool call completed: {events:#?}"
    );

    // The graph instance handed to the session now holds the snapshot the tool published.
    let listed = graph
        .find_callables(
            FindCallablesRequest {
                snapshot: None,
                language: None,
                name: None,
                path: None,
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .expect("a snapshot was published through the session's tool");
    assert_eq!(listed.page.items.len(), 2);
}

#[tokio::test]
async fn session_without_a_graph_does_not_run_graph_tools() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), "def a():\n    pass\n").unwrap();
    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    let session = SessionBuilder::new()
        .toolset(toolset(&["code_graph.refresh"]), workspace)
        .backend(backend("code_graph.refresh", serde_json::json!({})))
        .start()
        .await
        .expect("session builds");
    let mut subscriber = session.subscribe();
    let events = wait_for_tool_completion(&session, &mut subscriber).await;
    let summary = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("the call is answered");
    assert!(
        summary.has_error,
        "the graph tool must fail without an authorized graph: {summary:?}"
    );
}

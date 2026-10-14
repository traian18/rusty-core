use harness_code_analysis::{CallGraph, DefaultCallGraph};
use harness_tool_code_analysis::tools;
use harness_tools::{CancellationToken, ToolInput};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn graph_tool_descriptors_are_schema_backed_and_invalid_requests_are_structured() {
    let root = std::env::temp_dir().join(format!("graph-tool-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let graph: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
    let tools = tools(graph);
    assert_eq!(tools.len(), 5);
    for tool in &tools {
        let descriptor = tool.descriptor();
        assert!(descriptor.input_schema.get("type").is_some());
    }
    let find = tools
        .iter()
        .find(|tool| tool.descriptor().name == "code_graph.find_callables")
        .unwrap();
    let result = find
        .execute(
            ToolInput {
                arguments: json!({"limit": 0}),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.output["error"]["code"], "snapshot_not_found");
    assert_eq!(result.output["schema_version"], 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_graph_tool_returns_consistent_error_envelope() {
    let root = std::env::temp_dir().join(format!("graph-tool-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let graph: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
    let refresh = tools(graph)
        .into_iter()
        .find(|tool| tool.descriptor().name == "code_graph.refresh")
        .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = refresh
        .execute(
            ToolInput {
                arguments: json!({}),
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.output["error"]["code"], "cancelled");
    std::fs::remove_dir_all(root).unwrap();
}

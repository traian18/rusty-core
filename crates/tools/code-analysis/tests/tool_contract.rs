use harness_code_analysis::{CallGraph, CallGraphError, DefaultCallGraph};
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
    assert_eq!(
        result.output["error"]["code"], "invalid_request",
        "limit 0 is invalid"
    );
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

mod fake {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_code_analysis::*;
    use tokio_util::sync::CancellationToken;

    /// A `CallGraph` with canned answers: tool wrappers must work against any implementation.
    pub struct FakeGraph {
        pub seen: Mutex<Vec<String>>,
        pub fail_with: Mutex<Option<CallGraphError>>,
        pub partial: bool,
    }

    fn metadata(partial: bool) -> QueryMetadata {
        QueryMetadata {
            snapshot: SnapshotId("snap-fake".into()),
            partial,
            diagnostics: Vec::new(),
            truncated: false,
            limits: vec!["fake".into()],
            freshness: "fake".into(),
            generated_at_unix: 1,
        }
    }

    impl FakeGraph {
        pub fn new(partial: bool) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                fail_with: Mutex::new(None),
                partial,
            }
        }
        fn note(&self, what: &str) -> Result<(), CallGraphError> {
            self.seen.lock().unwrap().push(what.to_owned());
            match self.fail_with.lock().unwrap().clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    #[async_trait]
    impl CallGraph for FakeGraph {
        async fn refresh(
            &self,
            request: RefreshRequest,
            _c: CancellationToken,
        ) -> Result<RefreshResponse, CallGraphError> {
            self.note(&format!("refresh max_files={:?}", request.max_files))?;
            Ok(RefreshResponse {
                snapshot: SnapshotId("snap-fake".into()),
                metadata: metadata(self.partial),
                stats: RefreshStats::default(),
            })
        }
        fn capabilities(&self) -> CallGraphCapabilities {
            CallGraphCapabilities {
                languages: vec![LanguageCapabilities {
                    language: LanguageId::Rust,
                    supported: true,
                    limitations: vec![],
                    adapter_version: "fake-adapter".into(),
                    resolver_version: "fake-resolver".into(),
                }],
                max_page_size: 7,
                max_traversal_depth: 3,
                supports_partial_results: true,
                supports_cross_file_resolution: false,
            }
        }
        async fn find_callables(
            &self,
            request: FindCallablesRequest,
            _c: CancellationToken,
        ) -> Result<CallableResponse, CallGraphError> {
            self.note(&format!(
                "find name={:?} limit={}",
                request.name, request.limit
            ))?;
            Ok(CallableResponse {
                page: Page {
                    items: vec![],
                    next_cursor: Some("snap-fake:5".into()),
                    metadata: QueryMetadata {
                        truncated: true,
                        ..metadata(self.partial)
                    },
                },
            })
        }
        async fn calls_from(
            &self,
            request: CallsFromRequest,
            _c: CancellationToken,
        ) -> Result<CallsResponse, CallGraphError> {
            self.note(&format!(
                "from {} {:?}",
                request.caller.0, request.resolutions
            ))?;
            Ok(CallsResponse {
                page: Page {
                    items: vec![],
                    next_cursor: None,
                    metadata: metadata(self.partial),
                },
            })
        }
        async fn calls_to(
            &self,
            request: CallsToRequest,
            _c: CancellationToken,
        ) -> Result<CallsResponse, CallGraphError> {
            self.note(&format!("to {}", request.target.0))?;
            Ok(CallsResponse {
                page: Page {
                    items: vec![],
                    next_cursor: None,
                    metadata: metadata(self.partial),
                },
            })
        }
        async fn traverse(
            &self,
            request: TraversalRequest,
            _c: CancellationToken,
        ) -> Result<TraversalResponse, CallGraphError> {
            self.note(&format!(
                "traverse {:?} depth={}",
                request.direction, request.depth
            ))?;
            Ok(TraversalResponse {
                nodes: Page {
                    items: vec![],
                    next_cursor: None,
                    metadata: metadata(self.partial),
                },
                edges: Page {
                    items: vec![],
                    next_cursor: None,
                    metadata: metadata(self.partial),
                },
                truncated_by: vec![TraversalLimit::Depth],
            })
        }
    }
}

use fake::FakeGraph;
use harness_tools::ToolExecutor;

fn tool_named(graph: Arc<dyn CallGraph>, name: &str) -> Arc<dyn ToolExecutor> {
    tools(graph)
        .into_iter()
        .find(|tool| tool.descriptor().name == name)
        .unwrap()
}

async fn run(
    tool: &Arc<dyn ToolExecutor>,
    arguments: serde_json::Value,
) -> harness_tools::ToolResult {
    tool.execute(ToolInput { arguments }, CancellationToken::new())
        .await
        .unwrap()
}

#[tokio::test]
async fn wrappers_depend_only_on_the_call_graph_contract_and_return_versioned_envelopes() {
    let fake = Arc::new(FakeGraph::new(false));
    let graph: Arc<dyn CallGraph> = fake.clone();
    let refresh = run(
        &tool_named(graph.clone(), "code_graph.refresh"),
        json!({"max_files": 5}),
    )
    .await;
    assert!(!refresh.is_error);
    assert_eq!(refresh.output["schema_version"], 1);
    assert_eq!(refresh.output["partial"], false);
    assert_eq!(refresh.output["result"]["snapshot"], "snap-fake");
    assert_eq!(
        refresh.output["adapter_versions"][0]["adapter_version"],
        "fake-adapter"
    );
    assert_eq!(
        refresh.output["adapter_versions"][0]["resolver_version"],
        "fake-resolver"
    );

    let caps = run(
        &tool_named(graph.clone(), "code_graph.capabilities"),
        json!({}),
    )
    .await;
    assert_eq!(caps.output["result"]["max_page_size"], 7);

    let find = run(
        &tool_named(graph.clone(), "code_graph.find_callables"),
        json!({"name": "run", "limit": 3}),
    )
    .await;
    assert_eq!(find.output["result"]["page"]["next_cursor"], "snap-fake:5");
    assert_eq!(find.output["result"]["page"]["metadata"]["truncated"], true);

    run(
        &tool_named(graph.clone(), "code_graph.calls"),
        json!({"snapshot":"snap-fake","caller":"x","resolutions":["Possible"],"limit":2}),
    )
    .await;
    run(
        &tool_named(graph.clone(), "code_graph.calls"),
        json!({"snapshot":"snap-fake","target":"y","limit":2}),
    )
    .await;
    let traverse = run(
        &tool_named(graph, "code_graph.traverse"),
        json!({"snapshot":"snap-fake","roots":["x"],"depth":2,"limit":5,"direction":"callers"}),
    )
    .await;
    assert_eq!(traverse.output["result"]["truncated_by"][0], "depth");
    assert_eq!(
        *fake.seen.lock().unwrap(),
        [
            "refresh max_files=Some(5)",
            "find name=Some(\"run\") limit=3",
            "from x [Possible]",
            "to y",
            "traverse Callers depth=2",
        ]
    );
}

#[tokio::test]
async fn partial_snapshots_are_flagged_in_the_envelope() {
    let graph: Arc<dyn CallGraph> = Arc::new(FakeGraph::new(true));
    let result = run(&tool_named(graph, "code_graph.refresh"), json!({})).await;
    assert!(!result.is_error);
    assert_eq!(
        result.output["partial"], true,
        "partial success is distinguishable from failure"
    );
}

#[tokio::test]
async fn contract_errors_map_to_stable_codes() {
    let fake = Arc::new(FakeGraph::new(false));
    let graph: Arc<dyn CallGraph> = fake.clone();
    let find = tool_named(graph, "code_graph.find_callables");
    for (error, code) in [
        (
            CallGraphError::SnapshotNotFound(harness_code_analysis::SnapshotId("snap-x".into())),
            "snapshot_not_found",
        ),
        (CallGraphError::Cancelled, "cancelled"),
        (
            CallGraphError::DependencyFailure("boom".into()),
            "dependency_failure",
        ),
        (
            CallGraphError::LimitExceeded("too deep".into()),
            "limit_exceeded",
        ),
        (
            CallGraphError::InvalidRequest("bad".into()),
            "invalid_request",
        ),
    ] {
        *fake.fail_with.lock().unwrap() = Some(error);
        let result = run(&find, json!({"limit": 1})).await;
        assert!(result.is_error);
        assert_eq!(result.output["error"]["code"], code);
        assert_eq!(result.output["schema_version"], 1);
        assert!(result.output["error"]["message"].as_str().unwrap().len() > 3);
    }
}

#[tokio::test]
async fn malformed_and_unknown_arguments_are_rejected_before_reaching_the_graph() {
    let fake = Arc::new(FakeGraph::new(false));
    let graph: Arc<dyn CallGraph> = fake.clone();
    for (name, arguments) in [
        ("code_graph.find_callables", json!({"limit": "three"})),
        (
            "code_graph.find_callables",
            json!({"limit": 1, "bogus": true}),
        ),
        ("code_graph.calls", json!({"snapshot": "s", "limit": 1})),
        (
            "code_graph.calls",
            json!({"snapshot": "s", "caller": "a", "target": "b", "limit": 1}),
        ),
        (
            "code_graph.traverse",
            json!({"snapshot": "s", "roots": "x", "depth": 1, "limit": 1}),
        ),
        (
            "code_graph.traverse",
            json!({"snapshot": "s", "roots": ["x"], "depth": 1, "limit": 1, "direction": "sideways"}),
        ),
        ("code_graph.refresh", json!({"max_files": -1})),
    ] {
        let result = run(&tool_named(graph.clone(), name), arguments.clone()).await;
        assert!(result.is_error, "{name} {arguments}");
        assert_eq!(
            result.output["error"]["code"], "invalid_request",
            "{name} {arguments}"
        );
    }
    assert!(
        fake.seen.lock().unwrap().is_empty(),
        "no invalid request reached the graph"
    );
}

#[tokio::test]
async fn real_service_responses_flow_through_the_wrappers_with_pagination() {
    let root = std::env::temp_dir().join(format!("graph-tool-real-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let body: String = (0..5)
        .map(|i| format!("def f{i}():\n    pass\n"))
        .collect::<String>()
        + "def go():\n    f0()\n    nothing()\n";
    std::fs::write(root.join("a.py"), body).unwrap();
    let graph: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
    let refresh = run(&tool_named(graph.clone(), "code_graph.refresh"), json!({})).await;
    assert!(!refresh.is_error, "{}", refresh.output);
    let snapshot = refresh.output["result"]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    let find = tool_named(graph.clone(), "code_graph.find_callables");
    let page1 = run(&find, json!({"snapshot": snapshot, "limit": 4})).await;
    assert_eq!(
        page1.output["result"]["page"]["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let cursor = page1.output["result"]["page"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let page2 = run(
        &find,
        json!({"snapshot": snapshot, "limit": 4, "cursor": cursor}),
    )
    .await;
    assert_eq!(
        page2.output["result"]["page"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let go = run(
        &find,
        json!({"snapshot": snapshot, "name": "go", "limit": 1}),
    )
    .await;
    let go_id = go.output["result"]["page"]["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let calls = run(
        &tool_named(graph.clone(), "code_graph.calls"),
        json!({"snapshot": snapshot, "caller": go_id, "limit": 10}),
    )
    .await;
    let items = calls.output["result"]["page"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|c| c["resolution"] == "Resolved" && c["evidence"]["rule_id"] == "same-file-lexical"));
    assert!(items.iter().any(|c| c["resolution"] == "Unresolved"));
    let traverse = run(
        &tool_named(graph, "code_graph.traverse"),
        json!({"snapshot": snapshot, "roots": [go_id], "depth": 1, "limit": 10}),
    )
    .await;
    assert_eq!(
        traverse.output["result"]["nodes"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    std::fs::remove_dir_all(root).unwrap();
}

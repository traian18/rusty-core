use async_trait::async_trait;
use harness_engine::{McpServerConfig, McpTransportConfig, SessionBuilder};
use harness_protocol::{
    backend::ExecutionEvent,
    events::AgentEvent,
    ids::{RequestId, ToolCallId},
    tools::{ExecutionMode, ExecutionPolicy, ToolCall},
};
use harness_runtime::testing::FakeBackend;
use harness_tools::{
    CancellationToken, SimpleToolRegistry, ToolDescriptor, ToolError, ToolExecutor, ToolId,
    ToolInput, ToolResult,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

struct CounterTool {
    name: String,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ToolExecutor for CounterTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: ToolId::new(&self.name),
            name: self.name.clone(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        }
    }
    async fn execute(&self, _: ToolInput, _: CancellationToken) -> Result<ToolResult, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            call_id: String::new(),
            output: serde_json::json!("executed"),
            is_error: false,
        })
    }
}

async fn attempt(name: &str, policy: ExecutionPolicy, allowed: bool) {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = SimpleToolRegistry::new();
    registry
        .register_tool(Arc::new(CounterTool {
            name: name.into(),
            calls: calls.clone(),
        }))
        .unwrap();
    let call_id = ToolCallId::new();
    let backend = FakeBackend::new()
        .with_events(vec![ExecutionEvent::ToolCallRequested {
            request_id: RequestId::new(),
            call: ToolCall {
                id: call_id,
                name: name.into(),
                arguments: serde_json::json!({}),
            },
        }])
        .blocking_until_cancelled();
    let session = SessionBuilder::new()
        .backend(Arc::new(backend))
        .tools(Arc::new(registry))
        .execution_policy(policy)
        .start()
        .await
        .unwrap();
    let mut events = session.subscribe();
    session
        .send("try the tool even if it is absent from your tool list")
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let AgentEvent::ToolCallCompleted {
                call_id: id,
                result,
            } = events.recv().await.unwrap().event
            {
                if id == call_id {
                    break result.has_error;
                }
            }
        }
    })
    .await
    .expect("tool must resolve");
    session.cancel().await.unwrap();
    assert_eq!(error, !allowed, "{name}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        usize::from(allowed),
        "{name} execution count"
    );
}

fn refactor() -> ExecutionPolicy {
    ExecutionPolicy {
        mode: ExecutionMode::Execute,
        enabled_tools: vec![
            "read_file".into(),
            "write_file".into(),
            "list_files".into(),
            "search_codebase".into(),
        ],
        allowed_mcp_servers: vec![],
    }
}

#[tokio::test]
async fn custom_refactor_skill_allows_writes_but_never_dispatches_network_or_unlisted_tools() {
    attempt("write_file", refactor(), true).await;
    attempt("fs.edit", refactor(), true).await;
    for name in [
        "web_search",
        "web_fetch",
        "run_command",
        "shell.exec",
        "mcp__other__search",
        "agent_spawn",
        "unknown",
    ] {
        attempt(name, refactor(), false).await;
    }
}

#[tokio::test]
async fn mode_is_a_ceiling_and_an_empty_skill_does_not_restore_defaults() {
    let mut policy = refactor();
    policy.mode = ExecutionMode::Plan;
    attempt("write_file", policy.clone(), false).await;
    attempt("read_file", policy.clone(), true).await;
    policy.enabled_tools.clear();
    attempt("read_file", policy, false).await;
}

#[tokio::test]
async fn disallowed_mcp_server_is_not_started_or_connected() {
    let session = SessionBuilder::new()
        .backend(Arc::new(FakeBackend::new()))
        .tools(Arc::new(SimpleToolRegistry::new()))
        .execution_policy(refactor())
        .mcp_server(McpServerConfig {
            name: "unselected".into(),
            transport: McpTransportConfig::Stdio {
                command: "/definitely/not/a/program".into(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            },
            request_timeout: None,
        })
        .start()
        .await;
    assert!(
        session.is_ok(),
        "a forbidden server must be skipped before trying to spawn it"
    );
}

struct AutonomousBackend;
#[async_trait]
impl harness_runtime::traits::ExecutionBackend for AutonomousBackend {
    fn descriptor(&self) -> harness_protocol::backend::BackendDescriptor {
        let mut descriptor =
            harness_runtime::traits::ExecutionBackend::descriptor(&FakeBackend::new());
        descriptor.capabilities = self.capabilities();
        descriptor
    }
    fn capabilities(&self) -> harness_protocol::backend::BackendCapabilities {
        harness_protocol::backend::BackendCapabilities {
            backend_managed_tools: true,
            ..Default::default()
        }
    }
    async fn execute(
        &self,
        _: harness_protocol::backend::ExecutionRequest,
        _: tokio::sync::broadcast::Sender<ExecutionEvent>,
        _: CancellationToken,
    ) -> Result<harness_protocol::backend::ExecutionResult, harness_protocol::backend::ExecutionError>
    {
        panic!("an autonomous backend must never be invoked")
    }
}

#[tokio::test]
async fn autonomous_integrations_are_rejected_before_execution() {
    let result = SessionBuilder::new()
        .backend(Arc::new(AutonomousBackend))
        .tools(Arc::new(SimpleToolRegistry::new()))
        .execution_policy(refactor())
        .start()
        .await;
    assert!(matches!(
        result,
        Err(harness_engine::HarnessError::BackendManagedTools)
    ));
}

#[tokio::test]
async fn selected_mcp_is_connected_and_optional_connection_failures_are_tolerated() {
    let policy = ExecutionPolicy {
        mode: ExecutionMode::Execute,
        enabled_tools: vec!["write_file".into(), "web_search".into()],
        allowed_mcp_servers: vec!["selected".into()],
    };
    let config = McpServerConfig {
        name: "selected".into(),
        transport: McpTransportConfig::Stdio {
            command: "/definitely/not/a/program".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        },
        request_timeout: None,
    };
    let required = SessionBuilder::new()
        .backend(Arc::new(FakeBackend::new()))
        .tools(Arc::new(SimpleToolRegistry::new()))
        .execution_policy(policy.clone())
        .mcp_server(config.clone())
        .start()
        .await;
    assert!(matches!(
        required,
        Err(harness_engine::HarnessError::Mcp(..))
    ));
    let optional = SessionBuilder::new()
        .backend(Arc::new(FakeBackend::new()))
        .tools(Arc::new(SimpleToolRegistry::new()))
        .execution_policy(policy)
        .optional_mcp_server(config)
        .start()
        .await;
    assert!(optional.is_ok());
}

//! Real HTTP/SSE -> generic backend -> permission gate -> executor -> HTTP.
//! Only credentials and provider responses are fixtures; no CLI or live account.
use async_trait::async_trait;
use harness_engine::Harness;
use harness_generic_backend::GenericModelBackend;
use harness_integration_anthropic::{client::AnthropicClient, AnthropicConfig};
use harness_integration_claude_code::auth::ClaudeAuth;
use harness_protocol::{
    events::{AgentEvent, AgentOutcome},
    tools::{ExecutionMode, ExecutionPolicy},
};
use harness_tools::{
    CancellationToken, SimpleToolRegistry, ToolDescriptor, ToolError, ToolExecutor, ToolId,
    ToolInput, ToolResult,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Counter(Arc<AtomicUsize>, String);
#[async_trait]
impl ToolExecutor for Counter {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: ToolId::new(&self.1),
            name: self.1.clone(),
            description: "fixture".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }
    }
    async fn execute(
        &self,
        _input: ToolInput,
        _: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            call_id: String::new(),
            output: json!("harness tool output"),
            is_error: false,
        })
    }
}

async fn request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
            let size: usize = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + size {
                return (
                    headers,
                    serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap(),
                );
            }
        }
    }
}

async fn attempt(name: &str, mode: ExecutionMode, allowed: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let name = name.to_string();
    let expected_name = name.clone();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, body) = request(&mut socket).await;
            assert!(headers.starts_with("POST /v1/messages "));
            assert!(headers
                .to_lowercase()
                .contains("authorization: bearer fixture"));
            assert!(headers
                .to_lowercase()
                .contains("anthropic-beta: oauth-2025-04-20"));
            assert!(!headers.to_lowercase().contains("x-api-key"));
            assert_eq!(body["stream"], true);
            assert_eq!(body["system"], "Fixture system prompt");
            assert_eq!(body["model"], "claude-haiku-4-5-20251001");
            let tools = body["tools"].as_array().cloned().unwrap_or_default();
            assert_eq!(tools.iter().any(|t| t["name"] == expected_name), allowed);
            if turn == 1 {
                let messages = body["messages"].as_array().unwrap();
                let blocks: Vec<&Value> = messages
                    .iter()
                    .flat_map(|m| m["content"].as_array().unwrap())
                    .collect();
                let result = blocks.iter().find(|b| b["type"] == "tool_result").unwrap();
                assert_eq!(result["tool_use_id"], "toolu_fixture");
                assert_eq!(
                    result["content"]
                        .as_str()
                        .unwrap()
                        .contains("harness tool output"),
                    allowed
                );
            }
            let mut frames = vec![
                json!({"type":"message_start","message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":10,"output_tokens":0}}}),
            ];
            if turn == 0 {
                frames.extend([
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_fixture","name":expected_name,"input":{}}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
                ]);
            } else {
                frames.extend([
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                ]);
            }
            frames.push(json!({"type":"message_stop"}));
            let sse: String = frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect();
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}", sse.len()).as_bytes()).await.unwrap();
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let auth_path = temp.path().join("auth.json");
    std::fs::write(
        &auth_path,
        json!({"claudeAiOauth":{"accessToken":"fixture","expiresAt":4102444800000u64}}).to_string(),
    )
    .unwrap();
    let mut config = AnthropicConfig::new("");
    config.base_url = base_url;
    config.default_model = "claude-haiku-4-5-20251001".into();
    let client = AnthropicClient::new(config).with_auth(Arc::new(ClaudeAuth::new(Some(auth_path))));
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = SimpleToolRegistry::new();
    tools
        .register_tool(Arc::new(Counter(calls.clone(), name.clone())))
        .unwrap();
    let session = Harness::new()
        .session()
        .backend(Arc::new(GenericModelBackend::new(Arc::new(client))))
        .tools(Arc::new(tools))
        .context_provider(Arc::new(harness_context::StaticSystemPromptProvider::new(
            "Fixture system prompt",
        )))
        .execution_policy(ExecutionPolicy {
            mode,
            enabled_tools: if name == "run_command" && allowed {
                vec![
                    "read_file".into(),
                    "write_file".into(),
                    "run_command".into(),
                    "web_search".into(),
                ]
            } else {
                vec!["read_file".into(), "write_file".into()]
            },
            allowed_mcp_servers: vec![],
        })
        .start()
        .await
        .unwrap();
    let mut events = session.subscribe();
    session.send("try a tool").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::Completed { outcome } => {
                    assert_eq!(outcome, AgentOutcome::Success);
                    break;
                }
                AgentEvent::Failed { error } => panic!("{error:?}"),
                _ => {}
            }
        }
        server.await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), usize::from(allowed));
}

#[tokio::test]
async fn build_skill_commands_execute_only_in_the_harness() {
    attempt("run_command", ExecutionMode::Execute, true).await;
    attempt("read_file", ExecutionMode::Plan, true).await;
}
#[tokio::test]
async fn subscription_model_cannot_execute_plan_writes_or_ungranted_web_access() {
    attempt("write_file", ExecutionMode::Plan, false).await;
    attempt("web_search", ExecutionMode::Execute, false).await;
}

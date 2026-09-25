//! Real HTTP/SSE -> generic backend -> permission gate -> executor -> HTTP.
//! Only credentials and provider responses are fixtures; no CLI or live account.
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use harness_engine::Harness;
use harness_generic_backend::GenericModelBackend;
use harness_integration_codex::auth::CodexAuth;
use harness_integration_openai_responses::{OpenAiResponsesClient, OpenAiResponsesConfig};
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
            assert!(headers.starts_with("POST /responses "));
            assert!(headers
                .to_lowercase()
                .contains("chatgpt-account-id: fixture-account"));
            assert!(headers.to_lowercase().contains("authorization: bearer "));
            assert_eq!(body["store"], false);
            assert_eq!(body["stream"], true);
            assert_eq!(body["instructions"], "Fixture system prompt");
            assert_eq!(body["model"], "gpt-5.5");
            assert!(body.get("temperature").is_none());
            assert!(body.get("max_output_tokens").is_none());
            let tools = body["tools"].as_array().unwrap();
            assert_eq!(tools.iter().any(|t| t["name"] == expected_name), allowed);
            assert!(tools.iter().all(|t| t["type"] == "function"));
            if turn == 1 {
                let items = body["input"].as_array().unwrap();
                assert!(items
                    .iter()
                    .any(|i| i["type"] == "function_call" && i["call_id"] == "provider_call_1"));
                let result = items
                    .iter()
                    .find(|i| i["type"] == "function_call_output")
                    .unwrap();
                assert_eq!(result["call_id"], "provider_call_1");
                assert_eq!(
                    result["output"]
                        .as_str()
                        .unwrap()
                        .contains("harness tool output"),
                    allowed
                );
            }
            let frames = if turn == 0 {
                vec![
                    json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"provider_call_1","name":expected_name,"arguments":""}}),
                    json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"provider_call_1","name":expected_name,"arguments":"{}"}}),
                    json!({"type":"response.completed","response":{"model":"gpt-5.5","usage":{"input_tokens":10,"output_tokens":5}}}),
                ]
            } else {
                vec![
                    json!({"type":"response.output_text.delta","delta":"Done"}),
                    json!({"type":"response.completed","response":{"model":"gpt-5.5","usage":{"input_tokens":20,"output_tokens":2}}}),
                ]
            };
            let sse: String = frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect();
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}", sse.len()).as_bytes()).await.unwrap();
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let auth_path = temp.path().join("auth.json");
    let token = format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"exp":4102444800}"#)
    );
    std::fs::write(
        &auth_path,
        json!({"tokens":{"access_token":token,"account_id":"fixture-account"}}).to_string(),
    )
    .unwrap();
    let mut config = OpenAiResponsesConfig::new("");
    config.base_url = base_url;
    config.default_model = "gpt-5.5".into();
    let client = OpenAiResponsesClient::new(config)
        .with_chatgpt_auth(Arc::new(CodexAuth::new(Some(auth_path))));
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = SimpleToolRegistry::new();
    tools
        .register_tool(Arc::new(Counter(calls.clone(), name)))
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
            enabled_tools: vec!["read_file".into(), "write_file".into()],
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
async fn subscription_read_call_executes_in_harness_and_returns_result_to_model() {
    attempt("read_file", ExecutionMode::Plan, true).await;
}
#[tokio::test]
async fn subscription_model_cannot_execute_plan_writes_or_ungranted_web_access() {
    attempt("write_file", ExecutionMode::Plan, false).await;
    attempt("web_search", ExecutionMode::Execute, false).await;
}

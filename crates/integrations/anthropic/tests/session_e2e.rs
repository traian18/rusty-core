//! End-to-end Anthropic session test using a recorded SSE response.
//!
//! A loopback fixture server supplies deterministic HTTP bytes, so CI never
//! contacts Anthropic while the test still exercises the real client, wire
//! parser, generic backend, agent runtime, and public harness builder.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use harness_engine::Harness;
use harness_integration_anthropic::{AnthropicBackend, AnthropicConfig, AnthropicFactory};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_tools::{ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult};
use harness_tools::registry::ToolRegistry;

const TEXT_RESPONSE_SSE: &str = "\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fixture\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-20250513\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\
\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello, \"}}\n\
\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\
\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\
\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n\
\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\
";

struct NoTools;

#[async_trait]
impl ToolRegistry for NoTools {
    fn register(&self, _executor: Arc<dyn ToolExecutor>) -> Result<(), harness_tools::registry::RegistrationError> {
        Ok(())
    }

    fn get_executor(&self, _tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        None
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        Vec::new()
    }
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = socket.read(&mut chunk).await.expect("read fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= headers_end + 4 + content_length {
                break;
            }
        }
    }
    request
}

async fn start_fixture_server(bodies: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let address = listener.local_addr().expect("fixture server address");
    let expects_tool_round_trip = bodies.len() > 1;

    tokio::spawn(async move {
        for (index, body) in bodies.into_iter().enumerate() {
            let (mut socket, _) = listener.accept().await.expect("accept fixture request");
            let request = read_http_request(&mut socket).await;
            if expects_tool_round_trip {
                let request = String::from_utf8_lossy(&request);
                if index == 0 {
                    assert!(
                        request.contains("calculator"),
                        "initial request did not advertise the calculator tool: {request}"
                    );
                } else {
                    assert!(
                        request.contains("toolu_fixture"),
                        "follow-up request did not preserve Anthropic tool_use.id: {request}"
                    );
                }
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write fixture response");
            socket.shutdown().await.expect("close fixture response");
        }
    });

    format!("http://{address}")
}

#[tokio::test]
async fn anthropic_backend_runs_a_fixture_backed_session() {
    let base_url = start_fixture_server(vec![TEXT_RESPONSE_SSE]).await;
    let mut config = AnthropicConfig::new("fixture-key");
    config.base_url = base_url;
    config.request_timeout = Duration::from_secs(5);

    let session = Harness::new()
        .session()
        .backend(Arc::new(AnthropicBackend::new(config)))
        .tools(Arc::new(NoTools))
        .start()
        .await
        .expect("start Anthropic fixture session");

    let mut events = session.subscribe();
    session.send("Say hello").await.expect("send fixture prompt");

    let mut text = String::new();
    let mut completed = false;
    for _ in 0..32 {
        let envelope = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("session event timeout")
            .expect("session event stream closed");
        match envelope.event {
            AgentEvent::AssistantTextDelta { delta, .. } => text.push_str(&delta),
            AgentEvent::Completed { outcome } => {
                assert_eq!(outcome, AgentOutcome::Success);
                completed = true;
                break;
            }
            AgentEvent::Failed { error } => panic!("fixture session failed: {error:?}"),
            _ => {}
        }
    }

    assert!(completed, "fixture session did not complete");
    assert_eq!(text, "Hello, world!");

    let snapshot = session.snapshot();
    assert_eq!(snapshot.root_agent_status.metrics.total_tokens.value(), Some(15));
    assert_eq!(snapshot.usage.cumulative.total_requests, 1);
    assert!(snapshot.usage.cumulative.total_cost.is_some());
}


#[tokio::test]
async fn registry_factory_constructs_a_session() {
    let harness = Harness::new();
    harness
        .register_integration(Arc::new(AnthropicFactory))
        .expect("register Anthropic integration");

    let session = harness
        .session()
        .integration("anthropic", AnthropicConfig::new("fixture-key"))
        .expect("serialize Anthropic config")
        .tools(Arc::new(NoTools))
        .start()
        .await
        .expect("construct registry-backed Anthropic session");

    assert_ne!(session.session_id().to_string(), "");
}


const TOOL_RESPONSE_SSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"model":"claude-sonnet-4-20250513","usage":{"input_tokens":15,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_fixture","name":"calculator","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"expression\":\"2 + 2\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}

event: message_stop
data: {"type":"message_stop"}

"#;

const TOOL_FINAL_RESPONSE_SSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"model":"claude-sonnet-4-20250513","usage":{"input_tokens":9,"output_tokens":0}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"4"}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}

"#;

struct Calculator;

#[async_trait]
impl ToolExecutor for Calculator {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: ToolId::new("calculator"),
            name: "calculator".to_string(),
            description: "Evaluate a basic arithmetic expression".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"expression": {"type": "string"}},
                "required": ["expression"]
            }),
        }
    }

    async fn execute(
        &self,
        _input: ToolInput,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        // In a real calculator tool, we'd parse and evaluate the expression from _input.arguments
        Ok(ToolResult {
            call_id: "toolu_fixture".to_string(),
            output: serde_json::json!({"value": 4}),
            is_error: false,
        })
    }
}

struct CalculatorTools;

#[async_trait]
impl ToolRegistry for CalculatorTools {
    fn register(&self, _executor: Arc<dyn ToolExecutor>) -> Result<(), harness_tools::registry::RegistrationError> {
        Ok(())
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        (tool_id == "calculator").then(|| Arc::new(Calculator) as Arc<dyn ToolExecutor>)
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            id: ToolId::new("calculator"),
            name: "calculator".to_string(),
            description: "Evaluate a basic arithmetic expression".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"expression": {"type": "string"}},
                "required": ["expression"]
            }),
        }]
    }
}

#[tokio::test]
async fn anthropic_session_completes_a_tool_round_trip() {
    let base_url = start_fixture_server(vec![TOOL_RESPONSE_SSE, TOOL_FINAL_RESPONSE_SSE]).await;
    let mut config = AnthropicConfig::new("fixture-key");
    config.base_url = base_url;
    config.request_timeout = Duration::from_secs(5);

    let session = Harness::new()
        .session()
        .backend(Arc::new(AnthropicBackend::new(config)))
        .tools(Arc::new(CalculatorTools))
        .start()
        .await
        .expect("start tool fixture session");
    let mut events = session.subscribe();
    session.send("Calculate 2 + 2").await.expect("send tool prompt");

    let mut requested = false;
    let mut tool_completed = false;
    let mut final_text = String::new();
    let mut completed = false;
    for _ in 0..64 {
        let envelope = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("tool session event timeout")
            .expect("tool session event stream closed");
        match envelope.event {
            AgentEvent::ToolCallRequested { call } => {
                assert_eq!(call.name, "calculator");
                assert_eq!(call.arguments, serde_json::json!({"expression": "2 + 2"}));
                requested = true;
            }
            AgentEvent::ToolCallCompleted { .. } => tool_completed = true,
            AgentEvent::AssistantTextDelta { delta, .. } => final_text.push_str(&delta),
            AgentEvent::Completed { outcome } => {
                assert_eq!(outcome, AgentOutcome::Success);
                completed = true;
                break;
            }
            AgentEvent::Failed { error } => panic!("tool fixture session failed: {error:?}"),
            _ => {}
        }
    }

    assert!(requested, "model tool request was not emitted");
    assert!(tool_completed, "calculator result was not fed back to the session");
    assert_eq!(final_text, "4");
    assert!(completed, "tool fixture session did not complete");

    let snapshot = session.snapshot();
    assert_eq!(snapshot.root_agent_status.metrics.total_tokens.value(), Some(33));
    assert_eq!(snapshot.usage.cumulative.total_requests, 2);
}

//! End-to-end Gemini session test using a recorded SSE response.
//!
//! A loopback fixture server supplies deterministic HTTP bytes, so CI never
//! contacts Gemini while the test still exercises the real client, wire
//! parser, generic backend, agent runtime, and public harness builder.
//! Mirrors `crates/integrations/anthropic/tests/session_e2e.rs`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use harness_engine::Harness;
use harness_integration_gemini::{GeminiBackend, GeminiConfig, GeminiFactory};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_tools::registry::ToolRegistry;
use harness_tools::{ToolDescriptor, ToolExecutor};

const TEXT_RESPONSE_SSE: &str = "\
data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello, \"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":1,\"totalTokenCount\":11}}\n\
\ndata: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"world!\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":15}}\n\
\n";

struct NoTools;

#[async_trait]
impl ToolRegistry for NoTools {
    fn register(
        &self,
        _executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), harness_tools::registry::RegistrationError> {
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

async fn start_fixture_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let address = listener.local_addr().expect("fixture server address");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept fixture request");
        let request = read_http_request(&mut socket).await;
        let request_text = String::from_utf8_lossy(&request);
        assert!(
            request_text.contains("key=fixture-key"),
            "request did not carry the Gemini API key as a query parameter: {request_text}"
        );

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
    });

    format!("http://{address}")
}

#[tokio::test]
async fn gemini_backend_runs_a_fixture_backed_session() {
    let base_url = start_fixture_server(TEXT_RESPONSE_SSE).await;
    let mut config = GeminiConfig::new("fixture-key");
    config.base_url = base_url;
    config.request_timeout = Duration::from_secs(5);

    let session = Harness::new()
        .session()
        .backend(Arc::new(GeminiBackend::new(config)))
        .tools(Arc::new(NoTools))
        .start()
        .await
        .expect("start Gemini fixture session");

    let mut events = session.subscribe();
    session
        .send("Say hello")
        .await
        .expect("send fixture prompt");

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
    assert_eq!(
        snapshot.root_agent_status.metrics.total_tokens.value(),
        Some(15)
    );
    assert_eq!(snapshot.usage.cumulative.total_requests, 1);
    assert!(snapshot.usage.cumulative.total_cost.is_some());
}

#[tokio::test]
async fn registry_factory_constructs_a_session() {
    let harness = Harness::new();
    harness
        .register_integration(Arc::new(GeminiFactory))
        .expect("register Gemini integration");

    let session = harness
        .session()
        .integration("gemini", GeminiConfig::new("fixture-key"))
        .expect("serialize Gemini config")
        .tools(Arc::new(NoTools))
        .start()
        .await
        .expect("construct registry-backed Gemini session");

    assert_ne!(session.session_id().to_string(), "");
}

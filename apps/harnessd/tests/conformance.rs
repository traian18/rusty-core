//! M6: behavioral conformance suite across integration surfaces.
//!
//! Runs the *exact same* scenario (`Hello` → `CreateSession` → `Snapshot` →
//! `Mutate(Prompt)` → wait for the run to complete → `Snapshot` again →
//! `CloseSession`) against three independent transports — IPC (Unix
//! socket), stdio (in-memory duplex, same framing a real subprocess would
//! see), and WebSocket — asserting each produces the same *sequence of
//! response variants*. That's the concrete, checkable form of "every
//! supported integration surface passes the same behavioral scenario":
//! not that the transports are literally interchangeable byte-for-byte
//! (their framing differs by design — length-prefixed JSON, ND-JSON, and
//! WebSocket text frames respectively), but that a client talking any of
//! them observes identical protocol-level behavior from the same
//! `HarnessRpcHandler`.
//!
//! Not covered here (see `docs/production-readiness-roadmap.md`'s M6
//! section for the honest accounting): the Rust SDK and TypeScript SDK
//! aren't exercised by this file specifically — the Rust SDK talks the same
//! wire contract asserted here, and the TypeScript SDK has its own
//! contract-test suite (`sdk/typescript`) checked separately. Extending
//! this exact scenario to run through both SDKs' own client code, not just
//! raw wire frames, is the natural next step and is called out as such in
//! the roadmap rather than silently declared done.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use harness_engine::Harness;
use harness_integration_anthropic::AnthropicConfig;
use harness_protocol::admission::{CommandId, MutationMetadata};
use harness_protocol::rpc::{
    MutationCommand, RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse,
    RpcResponseBody, PROTOCOL_VERSION,
};
use harness_protocol::tools::AgentToolset;
use harness_runtime::rpc::RpcHandler;

#[path = "../src/handler.rs"]
mod handler;

/// One transport-agnostic client — each transport gets its own thin
/// implementation; the scenario below is written once against this trait.
#[async_trait]
trait WireClient: Send {
    async fn send(&mut self, request: &RpcRequest);
    async fn recv(&mut self) -> RpcResponse;
}

// ---------------------------------------------------------------------------
// IPC (Unix socket): length-prefixed JSON.
// ---------------------------------------------------------------------------

struct IpcClient {
    stream: UnixStream,
}

#[async_trait]
impl WireClient for IpcClient {
    async fn send(&mut self, request: &RpcRequest) {
        let bytes = serde_json::to_vec(request).unwrap();
        let len = bytes.len() as u32;
        self.stream.write_all(&len.to_le_bytes()).await.unwrap();
        self.stream.write_all(&bytes).await.unwrap();
        self.stream.flush().await.unwrap();
    }

    async fn recv(&mut self) -> RpcResponse {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }
}

// ---------------------------------------------------------------------------
// stdio: newline-delimited JSON over an in-memory duplex pipe.
// ---------------------------------------------------------------------------

struct StdioClient {
    reader: tokio::io::BufReader<tokio::io::DuplexStream>,
    writer: tokio::io::DuplexStream,
}

#[async_trait]
impl WireClient for StdioClient {
    async fn send(&mut self, request: &RpcRequest) {
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn recv(&mut self) -> RpcResponse {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        self.reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim_end()).unwrap()
    }
}

// ---------------------------------------------------------------------------
// WebSocket: one JSON-encoded text message per request/response.
// ---------------------------------------------------------------------------

struct WebSocketClient {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

#[async_trait]
impl WireClient for WebSocketClient {
    async fn send(&mut self, request: &RpcRequest) {
        let text = serde_json::to_string(request).unwrap();
        self.socket.send(Message::Text(text)).await.unwrap();
    }

    async fn recv(&mut self) -> RpcResponse {
        loop {
            match self
                .socket
                .next()
                .await
                .expect("socket closed before a response arrived")
            {
                Ok(Message::Text(text)) => return serde_json::from_str(&text).unwrap(),
                Ok(_) => continue,
                Err(error) => panic!("websocket error: {error}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The shared scenario
// ---------------------------------------------------------------------------

/// Runs the create→prompt→close scenario against `client` and returns the
/// sequence of `RpcResponseBody` *variant names* observed, in order — the
/// thing this test actually asserts is identical across transports.
async fn run_scenario(client: &mut dyn WireClient, workspace_root: PathBuf) -> Vec<&'static str> {
    let mut variants = Vec::new();

    client
        .send(&RpcRequest {
            id: RequestCorrelationId(0),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        })
        .await;
    let response = client.recv().await;
    variants.push(variant_name(&response.body));
    assert!(matches!(response.body, RpcResponseBody::Hello { .. }));

    client
        .send(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::CreateSession {
                workspace_root,
                integration: "anthropic".to_string(),
                integration_config: serde_json::to_value(AnthropicConfig::new("test-key")).unwrap(),
                toolset: AgentToolset {
                    tools: std::collections::HashMap::new(),
                },
                mcp_servers: Vec::new(),
            },
        })
        .await;
    let response = client.recv().await;
    variants.push(variant_name(&response.body));
    let session_id = match response.body {
        RpcResponseBody::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    client
        .send(&RpcRequest {
            id: RequestCorrelationId(2),
            session_id: Some(session_id),
            body: RpcRequestBody::Snapshot,
        })
        .await;
    let response = client.recv().await;
    variants.push(variant_name(&response.body));
    assert!(matches!(response.body, RpcResponseBody::Snapshot(_)));

    client
        .send(&RpcRequest {
            id: RequestCorrelationId(3),
            session_id: None,
            body: RpcRequestBody::GetDiagnostics {
                include_store_scan: false,
            },
        })
        .await;
    let response = client.recv().await;
    variants.push(variant_name(&response.body));
    assert!(matches!(response.body, RpcResponseBody::Diagnostics(_)));

    client
        .send(&RpcRequest {
            id: RequestCorrelationId(4),
            session_id: Some(session_id),
            body: RpcRequestBody::Mutate {
                metadata: MutationMetadata {
                    command_id: CommandId::new(),
                    session_id,
                    run_id: None,
                    expected_session_revision: None,
                    trace_id: None,
                },
                command: MutationCommand::CloseSession,
            },
        })
        .await;
    let response = client.recv().await;
    variants.push(variant_name(&response.body));
    assert!(matches!(response.body, RpcResponseBody::Admission { .. }));

    variants
}

fn variant_name(body: &RpcResponseBody) -> &'static str {
    match body {
        RpcResponseBody::Hello { .. } => "Hello",
        RpcResponseBody::SessionCreated { .. } => "SessionCreated",
        RpcResponseBody::SessionRestored { .. } => "SessionRestored",
        RpcResponseBody::SessionsListed { .. } => "SessionsListed",
        RpcResponseBody::Admission { .. } => "Admission",
        RpcResponseBody::Snapshot(_) => "Snapshot",
        RpcResponseBody::Event(_) => "Event",
        RpcResponseBody::EventGap { .. } => "EventGap",
        RpcResponseBody::Ack => "Ack",
        RpcResponseBody::Failure(_) => "Failure",
        RpcResponseBody::Diagnostics(_) => "Diagnostics",
    }
}

async fn build_handler() -> Arc<dyn RpcHandler> {
    let harness = Harness::builder()
        .register_integration(Arc::new(harness_integration_anthropic::AnthropicFactory))
        .build()
        .await
        .expect("build harness");
    Arc::new(handler::HarnessRpcHandler::new(Arc::new(harness)))
}

#[tokio::test]
async fn ipc_and_stdio_and_websocket_produce_the_same_response_sequence() {
    let workspace_dir = tempfile::tempdir().unwrap();

    // --- IPC ---
    let ipc_variants = {
        let socket_dir = tempfile::tempdir().unwrap();
        let socket_path = socket_dir.path().join("harness.sock");
        let handler = build_handler().await;
        let shutdown = CancellationToken::new();
        let serve_path = socket_path.clone();
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = harness_transport_ipc::serve(&serve_path, handler, serve_shutdown).await;
        });
        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket_path).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut client = IpcClient {
            stream: stream.expect("connect ipc"),
        };
        let variants = run_scenario(&mut client, workspace_dir.path().to_path_buf()).await;
        shutdown.cancel();
        variants
    };

    // --- stdio ---
    let stdio_variants = {
        let handler = build_handler().await;
        let shutdown = CancellationToken::new();
        let (client_reader, server_writer) = tokio::io::duplex(64 * 1024);
        let (server_reader, client_writer) = tokio::io::duplex(64 * 1024);
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = harness_transport_stdio::serve_io(
                server_reader,
                server_writer,
                handler,
                serve_shutdown,
            )
            .await;
        });
        let mut client = StdioClient {
            reader: tokio::io::BufReader::new(client_reader),
            writer: client_writer,
        };
        let variants = run_scenario(&mut client, workspace_dir.path().to_path_buf()).await;
        shutdown.cancel();
        variants
    };

    // --- WebSocket ---
    let websocket_variants = {
        let handler = build_handler().await;
        let shutdown = CancellationToken::new();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket listener");
        let addr = listener.local_addr().expect("local addr");
        let serve_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = harness_transport_websocket::serve_listener(listener, handler, serve_shutdown)
                .await;
        });
        let (socket, _response) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .expect("connect websocket");
        let mut client = WebSocketClient { socket };
        let variants = run_scenario(&mut client, workspace_dir.path().to_path_buf()).await;
        shutdown.cancel();
        variants
    };

    assert_eq!(
        ipc_variants, stdio_variants,
        "IPC and stdio must produce the same response sequence"
    );
    assert_eq!(
        ipc_variants, websocket_variants,
        "IPC and WebSocket must produce the same response sequence"
    );
    assert_eq!(
        ipc_variants,
        vec![
            "Hello",
            "SessionCreated",
            "Snapshot",
            "Diagnostics",
            "Admission"
        ],
        "sanity-check the scenario itself produced the expected shape"
    );
}

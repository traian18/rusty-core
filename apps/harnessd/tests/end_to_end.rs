//! End-to-end confidence test: a real `harness_transport_ipc::serve()` over a
//! real Unix socket, dispatching into a real `HarnessRpcHandler` wrapping a
//! real `Harness`. Everything below this (framing round-trips, handler
//! dispatch in isolation) is covered by faster unit tests in their own
//! crates; this one exists solely to prove the wiring between all three
//! layers actually works together.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use harness_engine::Harness;
use harness_integration_anthropic::AnthropicConfig;
use harness_protocol::rpc::{
    RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody,
    PROTOCOL_VERSION,
};
use harness_protocol::tools::AgentToolset;
use harness_runtime::rpc::RpcHandler;

#[path = "../src/handler.rs"]
mod handler;

async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) {
    let len = bytes.len() as u32;
    stream.write_all(&len.to_le_bytes()).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();
}

async fn read_frame(stream: &mut UnixStream) -> RpcResponse {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[tokio::test]
async fn full_stack_create_session_snapshot_close() {
    let sessions_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path: PathBuf = socket_dir.path().join("harness.sock");

    let harness = Harness::builder()
        .register_integration(Arc::new(harness_integration_anthropic::AnthropicFactory))
        .session_store(Arc::new(harness_session_store::JsonlSessionStore::new(
            sessions_dir.path(),
        )))
        .build()
        .await
        .expect("build harness");
    let rpc_handler: Arc<dyn RpcHandler> = Arc::new(handler::HarnessRpcHandler::new(Arc::new(harness)));

    let shutdown = CancellationToken::new();
    let serve_path = socket_path.clone();
    let serve_handler = rpc_handler.clone();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = harness_transport_ipc::serve(&serve_path, serve_handler, serve_shutdown).await;
    });

    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = UnixStream::connect(&socket_path).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stream = stream.expect("connect to harnessd's socket");

    // Every wire connection negotiates compatibility before session RPCs.
    let hello = RpcRequest {
        id: RequestCorrelationId(0),
        session_id: None,
        body: RpcRequestBody::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&hello).unwrap()).await;
    let response = read_frame(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Hello { .. }));

    // CreateSession
    let create = RpcRequest {
        id: RequestCorrelationId(1),
        session_id: None,
        body: RpcRequestBody::CreateSession {
            workspace_root: workspace_dir.path().to_path_buf(),
            integration: "anthropic".to_string(),
            integration_config: serde_json::to_value(AnthropicConfig::new("test-key")).unwrap(),
            toolset: AgentToolset {
                tools: std::collections::HashMap::new(),
            },
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&create).unwrap()).await;
    let response = read_frame(&mut stream).await;
    let session_id = match response.body {
        RpcResponseBody::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    // Snapshot
    let snapshot_req = RpcRequest {
        id: RequestCorrelationId(2),
        session_id: Some(session_id),
        body: RpcRequestBody::Snapshot,
    };
    write_frame(&mut stream, &serde_json::to_vec(&snapshot_req).unwrap()).await;
    let response = read_frame(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Snapshot(_)));

    // CloseSession
    let close_req = RpcRequest {
        id: RequestCorrelationId(3),
        session_id: Some(session_id),
        body: RpcRequestBody::CloseSession,
    };
    write_frame(&mut stream, &serde_json::to_vec(&close_req).unwrap()).await;
    let response = read_frame(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Ack));

    shutdown.cancel();
}

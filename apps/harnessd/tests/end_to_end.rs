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
use harness_protocol::admission::{CommandId, MutationMetadata};
use harness_protocol::rpc::{
    MutationCommand, RequestCorrelationId, RpcRequest, RpcRequestBody, RpcResponse,
    RpcResponseBody, PROTOCOL_VERSION,
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
    let rpc_handler: Arc<dyn RpcHandler> =
        Arc::new(handler::HarnessRpcHandler::new(Arc::new(harness)));

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
            mcp_servers: Vec::new(),
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
    };
    write_frame(&mut stream, &serde_json::to_vec(&close_req).unwrap()).await;
    let response = read_frame(&mut stream).await;
    // M1 re-verification (2026-08-07): a run-less mutation (close never
    // starts a run) must report the specific `AcceptedApplied`, not the
    // generic `Accepted` — see `HarnessRpcHandler::mutate`'s doc comment for
    // why this distinction exists and why prompt/steer/follow-up don't
    // (yet) get an equally specific `AcceptedStarted`/`AcceptedQueued`.
    match response.body {
        RpcResponseBody::Admission {
            result: harness_protocol::admission::AdmissionResult::AcceptedApplied,
            ..
        } => {}
        other => panic!("expected Admission{{AcceptedApplied}}, got {other:?}"),
    }

    shutdown.cancel();
}

/// M1 re-verification (2026-08-07): idempotent mutation admission — the
/// dedup cache and revision-conflict check are unit-tested in isolation
/// (`admission_cache_is_bounded_and_deduplicates` in `handler.rs`), but
/// nothing previously exercised them through the real `mutate()` RPC path
/// end to end. Proves: (1) replaying the exact same `command_id` returns
/// `Duplicate{original}` wrapping the first attempt's result, without
/// re-applying the mutation a second time; (2) a mutation carrying a stale
/// `expected_session_revision` is rejected with `RejectedConflict` and the
/// real current revision, rather than being silently applied or accepted.
#[tokio::test]
async fn mutation_admission_deduplicates_and_rejects_stale_revisions() {
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
    let rpc_handler: Arc<dyn RpcHandler> =
        Arc::new(handler::HarnessRpcHandler::new(Arc::new(harness)));

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

    let hello = RpcRequest {
        id: RequestCorrelationId(0),
        session_id: None,
        body: RpcRequestBody::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&hello).unwrap()).await;
    read_frame(&mut stream).await;

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
            mcp_servers: Vec::new(),
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&create).unwrap()).await;
    let session_id = match read_frame(&mut stream).await.body {
        RpcResponseBody::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    // First attempt: a Cancel mutation (a safe no-op-repeatable choice —
    // valid regardless of run state, no backend call needed) with the same
    // `command_id` sent twice.
    let shared_command_id = CommandId::new();
    let mutate_req = |correlation: u64, expected_session_revision: Option<u64>| RpcRequest {
        id: RequestCorrelationId(correlation),
        session_id: Some(session_id),
        body: RpcRequestBody::Mutate {
            metadata: MutationMetadata {
                command_id: shared_command_id,
                session_id,
                run_id: None,
                expected_session_revision,
                trace_id: None,
            },
            command: MutationCommand::Cancel,
        },
    };

    write_frame(
        &mut stream,
        &serde_json::to_vec(&mutate_req(2, None)).unwrap(),
    )
    .await;
    let first_result = match read_frame(&mut stream).await.body {
        RpcResponseBody::Admission {
            result,
            session_revision,
            ..
        } => {
            assert_eq!(
                session_revision, 1,
                "the first accepted mutation must advance the session revision"
            );
            result
        }
        other => panic!("expected Admission, got {other:?}"),
    };

    write_frame(
        &mut stream,
        &serde_json::to_vec(&mutate_req(3, None)).unwrap(),
    )
    .await;
    match read_frame(&mut stream).await.body {
        RpcResponseBody::Admission {
            result: harness_protocol::admission::AdmissionResult::Duplicate { original },
            session_revision,
            ..
        } => {
            assert_eq!(
                *original, first_result,
                "the duplicate must wrap the exact original result"
            );
            assert_eq!(
                session_revision, 1,
                "replaying a duplicate must not advance the revision a second time"
            );
        }
        other => panic!("expected Admission{{Duplicate}}, got {other:?}"),
    }

    // A fresh command with a stale expected revision (0, but the real
    // current revision is now 1) must be rejected, not silently applied.
    let stale_req = RpcRequest {
        id: RequestCorrelationId(4),
        session_id: Some(session_id),
        body: RpcRequestBody::Mutate {
            metadata: MutationMetadata {
                command_id: CommandId::new(),
                session_id,
                run_id: None,
                expected_session_revision: Some(0),
                trace_id: None,
            },
            command: MutationCommand::Cancel,
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&stale_req).unwrap()).await;
    match read_frame(&mut stream).await.body {
        RpcResponseBody::Admission {
            result:
                harness_protocol::admission::AdmissionResult::RejectedConflict {
                    current_session_revision,
                },
            ..
        } => {
            assert_eq!(
                current_session_revision, 1,
                "must report the real current revision, not the stale one"
            );
        }
        other => panic!("expected Admission{{RejectedConflict}}, got {other:?}"),
    }

    shutdown.cancel();
}

/// M6: proves `GetDiagnostics` works end-to-end through the real IPC
/// transport — real Prometheus recorder installed, real scheduler activity
/// (a session actually created) reflected in both the structured
/// `scheduler` field and the rendered Prometheus text, and the
/// `include_store_scan` flag actually changing the response shape.
#[tokio::test]
async fn get_diagnostics_reports_real_scheduler_and_metrics_state() {
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

    // `metrics`'s macros (`counter!`/`histogram!`/`gauge!`, used throughout
    // the runtime's instrumentation) always record into whichever recorder
    // is installed as the *process-wide global* — a recorder built but not
    // installed (`PrometheusBuilder::build_recorder`) never sees any of
    // that data, so this test needs the real `install_recorder()` path
    // `main.rs` itself uses, not a standalone handle. That call can only
    // succeed once per test *process*; this file has exactly one test that
    // needs it, so that's fine today — a second test needing its own
    // recorder in this binary would need `cargo test -- --test-threads=1`
    // and shared setup, or a separate test binary.
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("install the Prometheus recorder for this test process");

    let rpc_handler: Arc<dyn RpcHandler> = Arc::new(handler::HarnessRpcHandler::new_with_metrics(
        Arc::new(harness),
        Some(metrics_handle),
    ));

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

    // Create one session so scheduler/metrics have real activity to report.
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
            mcp_servers: Vec::new(),
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&create).unwrap()).await;
    let response = read_frame(&mut stream).await;
    assert!(matches!(
        response.body,
        RpcResponseBody::SessionCreated { .. }
    ));

    // Shallow diagnostics: no store scan.
    let diagnostics_req = RpcRequest {
        id: RequestCorrelationId(2),
        session_id: None,
        body: RpcRequestBody::GetDiagnostics {
            include_store_scan: false,
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&diagnostics_req).unwrap()).await;
    let response = read_frame(&mut stream).await;
    let snapshot = match response.body {
        RpcResponseBody::Diagnostics(snapshot) => snapshot,
        other => panic!("expected Diagnostics, got {other:?}"),
    };
    assert_eq!(
        snapshot.active_sessions, 1,
        "the session created above must be counted"
    );
    assert!(
        snapshot.store_scan.is_none(),
        "include_store_scan was false"
    );
    let session_permit = snapshot
        .scheduler
        .iter()
        .find(|permit| permit.kind == "session")
        .expect("a session permit entry must be present");
    assert_eq!(session_permit.in_use, 1);
    assert!(session_permit.capacity >= session_permit.in_use);
    assert!(
        snapshot
            .metrics_prometheus_text
            .contains("harness_scheduler_permit_wait_seconds"),
        "the real scheduler instrumentation must show up in the rendered Prometheus text: {}",
        snapshot.metrics_prometheus_text
    );
    assert!(
        snapshot
            .metrics_prometheus_text
            .contains("harness_scheduler_permits_in_use"),
        "in-use gauge must be present: {}",
        snapshot.metrics_prometheus_text
    );

    // Deep diagnostics: store scan included.
    let deep_req = RpcRequest {
        id: RequestCorrelationId(3),
        session_id: None,
        body: RpcRequestBody::GetDiagnostics {
            include_store_scan: true,
        },
    };
    write_frame(&mut stream, &serde_json::to_vec(&deep_req).unwrap()).await;
    let response = read_frame(&mut stream).await;
    let deep_snapshot = match response.body {
        RpcResponseBody::Diagnostics(snapshot) => snapshot,
        other => panic!("expected Diagnostics, got {other:?}"),
    };
    assert!(
        deep_snapshot.store_scan.is_some(),
        "include_store_scan was true"
    );

    shutdown.cancel();
}

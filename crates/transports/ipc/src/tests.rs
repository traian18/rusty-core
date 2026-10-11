use super::*;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
use tokio::sync::broadcast;

struct FakeRpcHandler {
    events: broadcast::Sender<AgentEventEnvelope>,
    known_session: SessionId,
    calls: Mutex<Vec<RpcRequestBody>>,
    backlog: Mutex<Vec<AgentEventEnvelope>>,
}

impl FakeRpcHandler {
    fn new(known_session: SessionId) -> Self {
        let (events, _) = broadcast::channel(16);
        Self {
            events,
            known_session,
            calls: Mutex::new(Vec::new()),
            backlog: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl RpcHandler for FakeRpcHandler {
    async fn handle(
        &self,
        _session_id: Option<SessionId>,
        body: RpcRequestBody,
    ) -> RpcResponseBody {
        self.calls.lock().unwrap().push(body.clone());
        match body {
            RpcRequestBody::CreateSession { .. } => RpcResponseBody::SessionCreated {
                session_id: self.known_session,
            },
            _ => RpcResponseBody::Ack,
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>> {
        if session_id == self.known_session {
            Some(self.events.subscribe())
        } else {
            None
        }
    }

    async fn events_since(&self, session_id: SessionId, since_seq: u64) -> Vec<AgentEventEnvelope> {
        if session_id != self.known_session {
            return Vec::new();
        }
        self.backlog
            .lock()
            .unwrap()
            .iter()
            .filter(|envelope| envelope.session_sequence.is_some_and(|seq| seq > since_seq))
            .cloned()
            .collect()
    }
}

#[allow(dead_code)]
fn make_envelope(session_id: SessionId, session_sequence: u64) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: EventId::new(),
        session_id,
        agent_id: AgentId::new(),
        parent_agent_id: None,
        run_id: Some(RunId::new()),
        agent_sequence: session_sequence,
        session_sequence: Some(session_sequence),
        timestamp: Timestamp::now(),
        visibility: EventVisibility::User,
        event: AgentEvent::RunStarted {
            run_id: RunId::new(),
        },
    }
}

async fn connect_and_serve(
    handler: Arc<FakeRpcHandler>,
) -> (UnixStream, tempfile::TempDir, CancellationToken) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("harness.sock");
    let shutdown = CancellationToken::new();

    let serve_path = socket_path.clone();
    let serve_handler: Arc<dyn RpcHandler> = handler;
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve(&serve_path, serve_handler, serve_shutdown).await;
    });

    // Give the listener a moment to bind before connecting.
    for _ in 0..50 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to freshly bound socket");
    (stream, dir, shutdown)
}

async fn send_request(stream: &mut UnixStream, request: &RpcRequest) {
    write_frame(stream, &serde_json::to_vec(request).unwrap())
        .await
        .expect("write request");
}

async fn read_response(stream: &mut UnixStream) -> RpcResponse {
    let bytes = read_frame(stream)
        .await
        .expect("read")
        .expect("some response");
    serde_json::from_slice(&bytes).unwrap()
}

async fn do_hello(stream: &mut UnixStream, id: u64) {
    send_request(
        stream,
        &RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(id),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;
    let response = read_response(stream).await;
    assert!(matches!(response.body, RpcResponseBody::Hello { .. }));
}

#[tokio::test]
async fn hello_negotiates_matching_protocol_version() {
    let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

    send_request(
        &mut stream,
        &RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;

    let response = read_response(&mut stream).await;
    match response.body {
        RpcResponseBody::Hello {
            protocol_version,
            capabilities,
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert!(capabilities.resumable_subscribe);
        }
        other => panic!("expected Hello, got {other:?}"),
    }
}

#[tokio::test]
async fn hello_rejects_a_mismatched_protocol_version() {
    let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

    send_request(
        &mut stream,
        &RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION + 1,
            },
        },
    )
    .await;

    let response = read_response(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));

    send_request(
        &mut stream,
        &RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(2),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Snapshot,
        },
    )
    .await;
    let response = read_response(&mut stream).await;
    assert!(matches!(
        response.body,
        RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
    ));
}

#[tokio::test]
async fn requests_before_hello_are_rejected() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;

    send_request(
        &mut stream,
        &RpcRequest {
            id: harness_protocol::rpc::RequestCorrelationId(1),
            session_id: Some(session_id),
            body: RpcRequestBody::Snapshot,
        },
    )
    .await;

    let response = read_response(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));

    // Hello still succeeds afterward on the same connection.
    do_hello(&mut stream, 2).await;
}

#[tokio::test]
async fn request_response_round_trips() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
    do_hello(&mut stream, 1).await;

    let request = RpcRequest {
        id: harness_protocol::rpc::RequestCorrelationId(2),
        session_id: None,
        body: RpcRequestBody::CreateSession {
            execution_policy: None,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
            integration: "anthropic".to_string(),
            integration_config: serde_json::json!({}),
            toolset: harness_protocol::tools::AgentToolset {
                tools: HashMap::new(),
            },
            mcp_servers: Vec::new(),
            skills: None,
        },
    };
    send_request(&mut stream, &request).await;

    let response = read_response(&mut stream).await;
    assert_eq!(
        response.id,
        Some(harness_protocol::rpc::RequestCorrelationId(2))
    );
    assert!(matches!(
        response.body,
        RpcResponseBody::SessionCreated { session_id: sid } if sid == session_id
    ));
}

#[tokio::test]
async fn subscribe_streams_events_after_ack() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    let events_tx = handler.events.clone();
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
    do_hello(&mut stream, 1).await;

    let subscribe = RpcRequest {
        id: harness_protocol::rpc::RequestCorrelationId(7),
        session_id: Some(session_id),
        body: RpcRequestBody::Subscribe { since_seq: None },
    };
    send_request(&mut stream, &subscribe).await;

    let ack = read_response(&mut stream).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    let envelope = make_envelope(session_id, 1);
    events_tx.send(envelope).expect("send event");

    let pushed = read_response(&mut stream).await;
    assert!(pushed.id.is_none());
    assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
}

#[tokio::test]
async fn subscribe_to_unknown_session_errors() {
    let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
    do_hello(&mut stream, 1).await;

    let subscribe = RpcRequest {
        id: harness_protocol::rpc::RequestCorrelationId(9),
        session_id: Some(SessionId::new()),
        body: RpcRequestBody::Subscribe { since_seq: None },
    };
    send_request(&mut stream, &subscribe).await;

    let response = read_response(&mut stream).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));
}

#[tokio::test]
async fn subscribe_with_since_seq_replays_backlog_before_live_events() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    {
        let mut backlog = handler.backlog.lock().unwrap();
        backlog.push(make_envelope(session_id, 1));
        backlog.push(make_envelope(session_id, 2));
    }
    let events_tx = handler.events.clone();
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
    do_hello(&mut stream, 1).await;

    let subscribe = RpcRequest {
        id: harness_protocol::rpc::RequestCorrelationId(3),
        session_id: Some(session_id),
        body: RpcRequestBody::Subscribe { since_seq: Some(0) },
    };
    send_request(&mut stream, &subscribe).await;

    let ack = read_response(&mut stream).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    let first = read_response(&mut stream).await;
    let second = read_response(&mut stream).await;
    let seqs: Vec<u64> = [first, second]
        .into_iter()
        .map(|r| match r.body {
            RpcResponseBody::Event(envelope) => envelope.session_sequence.unwrap(),
            other => panic!("expected replayed Event, got {other:?}"),
        })
        .collect();
    assert_eq!(seqs, vec![1, 2]);

    // A live event with a sequence already covered by the backlog is
    // deduplicated (never forwarded); one past the backlog is forwarded.
    events_tx
        .send(make_envelope(session_id, 2))
        .expect("send duplicate event");
    events_tx
        .send(make_envelope(session_id, 3))
        .expect("send fresh event");

    let third = read_response(&mut stream).await;
    match third.body {
        RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
        other => panic!("expected Event with seq 3, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_without_since_seq_skips_backlog() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    {
        let mut backlog = handler.backlog.lock().unwrap();
        backlog.push(make_envelope(session_id, 1));
    }
    let events_tx = handler.events.clone();
    let (mut stream, _dir, _shutdown) = connect_and_serve(handler).await;
    do_hello(&mut stream, 1).await;

    let subscribe = RpcRequest {
        id: harness_protocol::rpc::RequestCorrelationId(4),
        session_id: Some(session_id),
        body: RpcRequestBody::Subscribe { since_seq: None },
    };
    send_request(&mut stream, &subscribe).await;

    let ack = read_response(&mut stream).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    events_tx
        .send(make_envelope(session_id, 5))
        .expect("send live event");
    let pushed = read_response(&mut stream).await;
    match pushed.body {
        RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(5)),
        other => panic!("expected Event with seq 5, got {other:?}"),
    }
}

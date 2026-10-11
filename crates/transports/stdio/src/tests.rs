use super::*;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
use harness_protocol::rpc::RequestCorrelationId;
use tokio::io::AsyncReadExt;
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

async fn read_response_line<R: AsyncRead + Unpin>(reader: &mut R) -> RpcResponse {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await.expect("read byte");
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    serde_json::from_slice(&buf).expect("valid RpcResponse json")
}

async fn write_request<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, request: &RpcRequest) {
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    writer.write_all(&line).await.unwrap();
}

#[tokio::test]
async fn hello_negotiates_matching_protocol_version() {
    let session_id = SessionId::new();
    let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

    let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;

    let response = read_response_line(&mut client_out_read).await;
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
    let session_id = SessionId::new();
    let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

    let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION + 1,
            },
        },
    )
    .await;

    let response = read_response_line(&mut client_out_read).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(2),
            session_id: Some(session_id),
            body: RpcRequestBody::Snapshot,
        },
    )
    .await;
    let response = read_response_line(&mut client_out_read).await;
    assert!(matches!(
        response.body,
        RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
    ));
}

#[tokio::test]
async fn requests_before_hello_are_rejected() {
    let session_id = SessionId::new();
    let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

    let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);
    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: Some(session_id),
            body: RpcRequestBody::Snapshot,
        },
    )
    .await;

    let response = read_response_line(&mut client_out_read).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));
}

#[tokio::test]
async fn request_response_round_trips() {
    let session_id = SessionId::new();
    let handler: Arc<dyn RpcHandler> = Arc::new(FakeRpcHandler::new(session_id));

    let (client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    let mut client_in_write = client_in_write;
    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;
    let hello_response = read_response_line(&mut client_out_read).await;
    assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

    let request = RpcRequest {
        id: RequestCorrelationId(2),
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
    write_request(&mut client_in_write, &request).await;

    let response = read_response_line(&mut client_out_read).await;
    assert_eq!(response.id, Some(RequestCorrelationId(2)));
    assert!(matches!(
        response.body,
        RpcResponseBody::SessionCreated { session_id: sid } if sid == session_id
    ));
}

#[tokio::test]
async fn subscribe_streams_events_after_ack() {
    let session_id = SessionId::new();
    let handler_impl = Arc::new(FakeRpcHandler::new(session_id));
    let events_tx = handler_impl.events.clone();
    let handler: Arc<dyn RpcHandler> = handler_impl;

    let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;
    let hello_response = read_response_line(&mut client_out_read).await;
    assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(7),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: None },
        },
    )
    .await;

    let ack = read_response_line(&mut client_out_read).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    events_tx
        .send(make_envelope(session_id, 1))
        .expect("send event");

    let pushed = read_response_line(&mut client_out_read).await;
    assert!(pushed.id.is_none());
    assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
}

#[tokio::test]
async fn subscribe_with_since_seq_replays_backlog_and_dedupes_live_events() {
    let session_id = SessionId::new();
    let handler_impl = Arc::new(FakeRpcHandler::new(session_id));
    {
        let mut backlog = handler_impl.backlog.lock().unwrap();
        backlog.push(make_envelope(session_id, 1));
        backlog.push(make_envelope(session_id, 2));
    }
    let events_tx = handler_impl.events.clone();
    let handler: Arc<dyn RpcHandler> = handler_impl;

    let (mut client_in_write, server_in_read) = tokio::io::duplex(4096);
    let (server_out_write, mut client_out_read) = tokio::io::duplex(4096);

    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_io(server_in_read, server_out_write, handler, serve_shutdown).await;
    });

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        },
    )
    .await;
    let hello_response = read_response_line(&mut client_out_read).await;
    assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

    write_request(
        &mut client_in_write,
        &RpcRequest {
            id: RequestCorrelationId(3),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: Some(0) },
        },
    )
    .await;

    let ack = read_response_line(&mut client_out_read).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    let first = read_response_line(&mut client_out_read).await;
    let second = read_response_line(&mut client_out_read).await;
    let seqs: Vec<u64> = [first, second]
        .into_iter()
        .map(|r| match r.body {
            RpcResponseBody::Event(envelope) => envelope.session_sequence.unwrap(),
            other => panic!("expected replayed Event, got {other:?}"),
        })
        .collect();
    assert_eq!(seqs, vec![1, 2]);

    events_tx
        .send(make_envelope(session_id, 2))
        .expect("send duplicate event");
    events_tx
        .send(make_envelope(session_id, 3))
        .expect("send fresh event");

    let third = read_response_line(&mut client_out_read).await;
    match third.body {
        RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
        other => panic!("expected Event with seq 3, got {other:?}"),
    }
}

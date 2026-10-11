use super::*;
use std::collections::HashMap;

use std::sync::Mutex;

use async_trait::async_trait;
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
use harness_protocol::ids::{AgentId, EventId, RunId, SessionId, Timestamp};
use harness_protocol::rpc::RequestCorrelationId;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

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

async fn start_server(handler: Arc<FakeRpcHandler>) -> (SocketAddr, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let shutdown = CancellationToken::new();
    let serve_handler: Arc<dyn RpcHandler> = handler;
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve_listener(listener, serve_handler, serve_shutdown).await;
    });
    (addr, shutdown)
}

async fn recv_response(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> RpcResponse {
    let message = ws.next().await.expect("some message").expect("ok");
    let text = message.into_text().expect("text message");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn hello_negotiates_matching_protocol_version() {
    let handler = Arc::new(FakeRpcHandler::new(SessionId::new()));
    let (addr, _shutdown) = start_server(handler).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        })
        .unwrap(),
    ))
    .await
    .expect("send hello");

    let response = recv_response(&mut ws).await;
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
    let (addr, _shutdown) = start_server(handler).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION + 1,
            },
        })
        .unwrap(),
    ))
    .await
    .expect("send hello");

    let response = recv_response(&mut ws).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(2),
            session_id: Some(SessionId::new()),
            body: RpcRequestBody::Snapshot,
        })
        .unwrap(),
    ))
    .await
    .expect("send request after rejected hello");
    let response = recv_response(&mut ws).await;
    assert!(matches!(
        response.body,
        RpcResponseBody::Failure(error) if error.message.contains("Hello must be the first")
    ));
}

#[tokio::test]
async fn requests_before_hello_are_rejected() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    let (addr, _shutdown) = start_server(handler).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: Some(session_id),
            body: RpcRequestBody::Snapshot,
        })
        .unwrap(),
    ))
    .await
    .expect("send");

    let response = recv_response(&mut ws).await;
    assert!(matches!(response.body, RpcResponseBody::Failure(_)));
}

#[tokio::test]
async fn request_response_round_trips() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    let (addr, _shutdown) = start_server(handler).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        })
        .unwrap(),
    ))
    .await
    .expect("send hello");
    let hello_response = recv_response(&mut ws).await;
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
    ws.send(Message::Text(serde_json::to_string(&request).unwrap()))
        .await
        .expect("send");

    let response = recv_response(&mut ws).await;
    assert_eq!(response.id, Some(RequestCorrelationId(2)));
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
    let (addr, _shutdown) = start_server(handler).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        })
        .unwrap(),
    ))
    .await
    .expect("send hello");
    let hello_response = recv_response(&mut ws).await;
    assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

    let subscribe = RpcRequest {
        id: RequestCorrelationId(7),
        session_id: Some(session_id),
        body: RpcRequestBody::Subscribe { since_seq: None },
    };
    ws.send(Message::Text(serde_json::to_string(&subscribe).unwrap()))
        .await
        .expect("send subscribe");

    let ack = recv_response(&mut ws).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    events_tx
        .send(make_envelope(session_id, 1))
        .expect("send event");

    let pushed = recv_response(&mut ws).await;
    assert!(pushed.id.is_none());
    assert!(matches!(pushed.body, RpcResponseBody::Event(_)));
}

#[tokio::test]
async fn subscribe_with_since_seq_replays_backlog_and_dedupes_live_events() {
    let session_id = SessionId::new();
    let handler = Arc::new(FakeRpcHandler::new(session_id));
    {
        let mut backlog = handler.backlog.lock().unwrap();
        backlog.push(make_envelope(session_id, 1));
        backlog.push(make_envelope(session_id, 2));
    }
    let events_tx = handler.events.clone();
    let (addr, _shutdown) = start_server(handler).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(1),
            session_id: None,
            body: RpcRequestBody::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        })
        .unwrap(),
    ))
    .await
    .expect("send hello");
    let hello_response = recv_response(&mut ws).await;
    assert!(matches!(hello_response.body, RpcResponseBody::Hello { .. }));

    ws.send(Message::Text(
        serde_json::to_string(&RpcRequest {
            id: RequestCorrelationId(3),
            session_id: Some(session_id),
            body: RpcRequestBody::Subscribe { since_seq: Some(0) },
        })
        .unwrap(),
    ))
    .await
    .expect("send subscribe");

    let ack = recv_response(&mut ws).await;
    assert!(matches!(ack.body, RpcResponseBody::Ack));

    let first = recv_response(&mut ws).await;
    let second = recv_response(&mut ws).await;
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

    let third = recv_response(&mut ws).await;
    match third.body {
        RpcResponseBody::Event(envelope) => assert_eq!(envelope.session_sequence, Some(3)),
        other => panic!("expected Event with seq 3, got {other:?}"),
    }
}

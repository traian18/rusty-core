//! End-to-end tests for the MCP server transport, driven over an in-memory
//! duplex pair against a fake `RpcHandler`.
//!
//! The fake deliberately emits a run's events **synchronously inside
//! `handle()`** for a `Mutate { Prompt }`. That is what makes
//! [`prompt_captures_events_emitted_immediately_after_admission`] a real
//! test: a subscriber created after the mutation returns would observe
//! nothing at all, because `broadcast::Receiver` only delivers messages sent
//! after it exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_protocol::admission::AdmissionResult;
use harness_protocol::commands::{AgentError, AgentStatus};
use harness_protocol::events::{AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility};
use harness_protocol::ids::{
    AgentId, EventId, MessageId, PermissionId, SessionId, Timestamp, ToolCallId,
};
use harness_protocol::rpc::{MutationCommand, RpcRequestBody, RpcResponseBody};
use harness_protocol::tools::{ToolCall, ToolResultSummary};
use harness_runtime::rpc::RpcHandler;
use harness_transport_mcp::{serve_io, McpServeConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Fake handler
// ---------------------------------------------------------------------------

/// What the fake should emit when a prompt is admitted.
#[derive(Clone)]
enum Script {
    /// Two text deltas, a tool call, then a successful completion.
    Successful,
    /// Text, then a permission request that never gets answered.
    BlocksOnPermission,
    /// Text, then a hard failure.
    Fails,
    /// Admit the prompt but emit nothing at all — exercises the timeout.
    Silent,
    /// Refuse the mutation outright.
    Reject,
}

struct FakeHandler {
    channels: Mutex<HashMap<SessionId, broadcast::Sender<AgentEventEnvelope>>>,
    durable: Mutex<HashMap<SessionId, Vec<AgentEventEnvelope>>>,
    script: Script,
    created: Mutex<Vec<SessionId>>,
}

impl FakeHandler {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            durable: Mutex::new(HashMap::new()),
            script,
            created: Mutex::new(Vec::new()),
        })
    }

    fn envelope(session_id: SessionId, sequence: u64, event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: None,
            agent_sequence: sequence,
            session_sequence: Some(sequence),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event,
        }
    }

    /// Publishes an event to live subscribers *and* the durable log.
    fn emit(&self, session_id: SessionId, sequence: u64, event: AgentEvent) {
        let envelope = Self::envelope(session_id, sequence, event);
        self.durable
            .lock()
            .unwrap()
            .entry(session_id)
            .or_default()
            .push(envelope.clone());
        if let Some(tx) = self.channels.lock().unwrap().get(&session_id) {
            let _ = tx.send(envelope);
        }
    }

    fn run_script(&self, session_id: SessionId) {
        match self.script {
            Script::Reject | Script::Silent => {}
            Script::Successful => {
                self.emit(
                    session_id,
                    1,
                    AgentEvent::StateChanged {
                        from: AgentStatus::Idle,
                        to: AgentStatus::Idle,
                    },
                );
                self.emit(
                    session_id,
                    2,
                    AgentEvent::AssistantTextDelta {
                        message_id: MessageId::new(),
                        delta: "Hello ".into(),
                    },
                );
                self.emit(
                    session_id,
                    3,
                    AgentEvent::AssistantTextDelta {
                        message_id: MessageId::new(),
                        delta: "world".into(),
                    },
                );
                self.emit(
                    session_id,
                    4,
                    AgentEvent::ToolCallCompleted {
                        call_id: ToolCallId::new(),
                        result: ToolResultSummary {
                            has_error: false,
                            output_preview: "read 3 files".into(),
                        },
                    },
                );
                self.emit(
                    session_id,
                    5,
                    AgentEvent::Completed {
                        outcome: AgentOutcome::Success,
                    },
                );
            }
            Script::BlocksOnPermission => {
                self.emit(
                    session_id,
                    1,
                    AgentEvent::AssistantTextDelta {
                        message_id: MessageId::new(),
                        delta: "I'll run that.".into(),
                    },
                );
                self.emit(
                    session_id,
                    2,
                    AgentEvent::PermissionRequested {
                        request: harness_protocol::effects::PermissionRequest {
                            id: PermissionId::new(),
                            tool_call: ToolCall {
                                id: ToolCallId::new(),
                                name: "shell.exec".into(),
                                arguments: json!({}),
                            },
                            agent_id: AgentId::new(),
                        },
                    },
                );
            }
            Script::Fails => {
                self.emit(
                    session_id,
                    1,
                    AgentEvent::AssistantTextDelta {
                        message_id: MessageId::new(),
                        delta: "partial".into(),
                    },
                );
                self.emit(
                    session_id,
                    2,
                    AgentEvent::Failed {
                        error: AgentError {
                            message: "backend exploded".into(),
                            code: "BACKEND_FAILED".into(),
                            details: None,
                        },
                    },
                );
            }
        }
    }
}

#[async_trait]
impl RpcHandler for FakeHandler {
    async fn handle(&self, session_id: Option<SessionId>, body: RpcRequestBody) -> RpcResponseBody {
        match body {
            RpcRequestBody::CreateSession { .. } => {
                let id = SessionId::new();
                self.channels
                    .lock()
                    .unwrap()
                    .insert(id, broadcast::channel(256).0);
                self.created.lock().unwrap().push(id);
                RpcResponseBody::SessionCreated { session_id: id }
            }
            RpcRequestBody::Mutate { metadata, command } => {
                let id = session_id.unwrap_or(metadata.session_id);
                if matches!(command, MutationCommand::Prompt(_)) {
                    if matches!(self.script, Script::Reject) {
                        return RpcResponseBody::Admission {
                            metadata,
                            result: AdmissionResult::RejectedInvalidState {
                                reason: "a run is already active".into(),
                            },
                            session_revision: 1,
                        };
                    }
                    // Emitted before this call returns: only a subscriber
                    // that already existed can see these.
                    self.run_script(id);
                }
                RpcResponseBody::Admission {
                    metadata,
                    result: AdmissionResult::Accepted,
                    session_revision: 1,
                }
            }
            RpcRequestBody::ListSessions => RpcResponseBody::SessionsListed {
                sessions: self
                    .created
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|id| harness_protocol::rpc::SessionSummaryWire {
                        session_id: *id,
                        title: "fake session".into(),
                        backend_name: None,
                        updated_at: Timestamp::now(),
                        restorable: true,
                    })
                    .collect(),
            },
            _ => RpcResponseBody::Ack,
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>> {
        self.channels
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|tx| tx.subscribe())
    }

    async fn events_since(&self, session_id: SessionId, since: u64) -> Vec<AgentEventEnvelope> {
        self.durable
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.session_sequence.unwrap_or(0) > since)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Harness for driving the server
// ---------------------------------------------------------------------------

struct Client {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    next_id: u64,
    _shutdown: CancellationToken,
}

impl Client {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write");
        self.writer.flush().await.expect("flush");

        let response = tokio::time::timeout(Duration::from_secs(10), self.reader.next_line())
            .await
            .expect("server should answer within 10s")
            .expect("read")
            .expect("a response line");
        let value: Value = serde_json::from_str(&response).expect("valid JSON response");
        assert_eq!(value["id"], id, "response id must match the request");
        value
    }

    /// Sends a notification, which must not be answered.
    async fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method});
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write");
        self.writer.flush().await.expect("flush");
    }

    async fn send_raw(&mut self, raw: &str) -> Value {
        self.writer
            .write_all(format!("{raw}\n").as_bytes())
            .await
            .expect("write");
        self.writer.flush().await.expect("flush");
        let response = tokio::time::timeout(Duration::from_secs(10), self.reader.next_line())
            .await
            .expect("server should answer")
            .expect("read")
            .expect("a response line");
        serde_json::from_str(&response).expect("valid JSON response")
    }
}

fn start(handler: Arc<FakeHandler>, timeout: Duration) -> Client {
    let (server_side, client_side) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);

    let shutdown = CancellationToken::new();
    let config = McpServeConfig::new("fake", "/tmp/ws").prompt_timeout(timeout);

    tokio::spawn(serve_io(
        server_read,
        server_write,
        handler as Arc<dyn RpcHandler>,
        config,
        shutdown.clone(),
    ));

    Client {
        writer: client_write,
        reader: BufReader::new(client_read).lines(),
        next_id: 0,
        _shutdown: shutdown,
    }
}

/// Runs the handshake and returns a fresh session id.
async fn handshake_and_create(client: &mut Client) -> String {
    client.request("initialize", json!({})).await;
    client.notify("notifications/initialized").await;
    let created = client
        .request(
            "tools/call",
            json!({"name": "harness_create_session", "arguments": {}}),
        )
        .await;
    let text = created["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    text.split_whitespace()
        .nth(2)
        .expect("session id in the message")
        .trim_end_matches('.')
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_advertises_only_implemented_capabilities() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    let response = client.request("initialize", json!({})).await;

    let result = &response["result"];
    assert_eq!(result["serverInfo"]["name"], "rusty-core");
    assert_eq!(
        result["protocolVersion"],
        harness_protocol::mcp::MCP_PROTOCOL_VERSION,
        "the server must advertise the same version the client claims"
    );
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
    // Advertising these would make clients call methods we answer
    // "method not found" to.
    assert!(result["capabilities"]["prompts"].is_null());
    assert!(result["capabilities"]["sampling"].is_null());
}

#[tokio::test]
async fn tools_list_returns_the_four_harness_tools() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    client.request("initialize", json!({})).await;

    let response = client.request("tools/list", json!({})).await;
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "harness_create_session",
            "harness_prompt",
            "harness_cancel",
            "harness_list_sessions"
        ]
    );
}

/// **The ordering test.** The fake emits the entire run inside `handle()`,
/// so these events exist only for a subscriber that was already attached.
/// If `prompt_and_wait` ever subscribes *after* mutating, this fails —
/// either with empty text or by hanging until the timeout.
#[tokio::test]
async fn prompt_captures_events_emitted_immediately_after_admission() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    let session_id = handshake_and_create(&mut client).await;

    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "hi"}
            }),
        )
        .await;

    let result = &response["result"];
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("Hello world"),
        "streamed deltas were not accumulated: {text}"
    );
    assert!(
        text.contains("read 3 files"),
        "tool-call summary missing: {text}"
    );
}

#[tokio::test]
async fn a_rejected_prompt_returns_promptly_instead_of_waiting_for_the_timeout() {
    // A 30s prompt timeout that must not be what unblocks us.
    let mut client = start(FakeHandler::new(Script::Reject), Duration::from_secs(30));
    let session_id = handshake_and_create(&mut client).await;

    let started = std::time::Instant::now();
    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "hi"}
            }),
        )
        .await;

    assert_eq!(response["result"]["isError"], true);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a rejected admission should return at once, took {:?}",
        started.elapsed()
    );
}

/// A permission prompt has no MCP-side answer channel. Reporting that is
/// far better than burning the whole timeout and saying "timed out".
#[tokio::test]
async fn a_permission_prompt_reports_the_cause_rather_than_hanging() {
    let mut client = start(
        FakeHandler::new(Script::BlocksOnPermission),
        Duration::from_secs(30),
    );
    let session_id = handshake_and_create(&mut client).await;

    let started = std::time::Instant::now();
    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "run something"}
            }),
        )
        .await;

    let result = &response["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("permission"), "{text}");
    assert!(text.contains("Allow"), "the remedy should be named: {text}");
    // Partial output is still returned rather than discarded.
    assert!(text.contains("I'll run that."), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should not have waited for the timeout, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_failed_run_surfaces_the_agent_error() {
    let mut client = start(FakeHandler::new(Script::Fails), Duration::from_secs(5));
    let session_id = handshake_and_create(&mut client).await;

    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "hi"}
            }),
        )
        .await;

    let result = &response["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("backend exploded"), "{text}");
}

#[tokio::test]
async fn a_silent_run_hits_the_prompt_timeout() {
    let mut client = start(FakeHandler::new(Script::Silent), Duration::from_millis(300));
    let session_id = handshake_and_create(&mut client).await;

    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "hi"}
            }),
        )
        .await;

    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("timed out"),
        "{result}"
    );
}

#[tokio::test]
async fn the_transcript_resource_renders_a_sessions_durable_events() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    let session_id = handshake_and_create(&mut client).await;
    client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": session_id, "prompt": "hi"}
            }),
        )
        .await;

    let listed = client.request("resources/list", json!({})).await;
    let uri = listed["result"]["resources"][0]["uri"]
        .as_str()
        .expect("a resource uri");
    assert_eq!(uri, format!("harness://session/{session_id}"));

    let read = client.request("resources/read", json!({"uri": uri})).await;
    let text = read["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("Hello world"), "{text}");
}

#[tokio::test]
async fn an_unknown_method_gets_a_json_rpc_method_not_found() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    let response = client.request("prompts/list", json!({})).await;
    assert_eq!(response["error"]["code"], -32601);
    assert!(response.get("result").is_none());
}

#[tokio::test]
async fn malformed_input_gets_a_well_formed_parse_error() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    let response = client.send_raw("{not json at all").await;
    assert_eq!(response["error"]["code"], -32700);
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["jsonrpc"], "2.0");
}

/// Per JSON-RPC a notification must never be answered. If the server did
/// reply, the next request's id assertion would see the stray line.
#[tokio::test]
async fn notifications_are_not_answered() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    client.request("initialize", json!({})).await;
    client.notify("notifications/initialized").await;
    client.notify("notifications/cancelled").await;

    let response = client.request("tools/list", json!({})).await;
    assert!(response["result"]["tools"].is_array());
}

/// Regression: a peer that pipes several requests and immediately closes
/// its end must still receive every reply.
///
/// The writer task used to exit on a cancellation signal that fired as soon
/// as the read loop saw EOF, which raced the queued replies and dropped
/// whatever hadn't been flushed yet. Driving the real binary with
/// `printf ... | harnessd --mcp-stdio` answered `initialize` and then went
/// silent on `tools/list`; the connected-client tests above never caught it
/// because they read each reply before closing.
#[tokio::test]
async fn every_reply_is_delivered_even_when_the_peer_closes_immediately() {
    let (server_side, client_side) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, mut client_write) = tokio::io::split(client_side);

    let shutdown = CancellationToken::new();
    let server = tokio::spawn(serve_io(
        server_read,
        server_write,
        FakeHandler::new(Script::Successful) as Arc<dyn RpcHandler>,
        McpServeConfig::new("fake", "/tmp/ws"),
        shutdown,
    ));

    // Three requests in one write, then close stdin immediately.
    let batch = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
    ]
    .iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    client_write
        .write_all(format!("{batch}\n").as_bytes())
        .await
        .expect("write");
    client_write.flush().await.expect("flush");
    // `shutdown()`, not `drop()`: with `tokio::io::split` both halves share
    // the underlying stream, so dropping one alone never signals EOF to the
    // peer and the server would wait forever. This is the EOF that used to
    // truncate the replies.
    client_write.shutdown().await.expect("shutdown");
    drop(client_write);

    let mut reader = BufReader::new(client_read).lines();
    let mut ids = Vec::new();
    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("valid JSON");
        ids.push(value["id"].as_u64().expect("an id"));
    }

    assert_eq!(
        ids,
        vec![1, 2, 3],
        "every queued reply must survive the peer closing its end"
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

#[tokio::test]
async fn a_bad_session_id_is_a_tool_error_not_a_protocol_error() {
    let mut client = start(FakeHandler::new(Script::Successful), Duration::from_secs(5));
    client.request("initialize", json!({})).await;

    let response = client
        .request(
            "tools/call",
            json!({
                "name": "harness_prompt",
                "arguments": {"session_id": "not-a-uuid", "prompt": "hi"}
            }),
        )
        .await;
    // A bad argument is something the caller can fix, so it comes back as a
    // tool result rather than a JSON-RPC error.
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true);
}

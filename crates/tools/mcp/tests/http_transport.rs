//! End-to-end tests for the streamable-HTTP transport, against a real
//! socket.
//!
//! The stub is a hand-rolled TCP listener speaking just enough HTTP/1.1
//! rather than a web framework: the transport's interesting behavior is in
//! *framing* — which content type came back, whether `Mcp-Session-Id` was
//! echoed, whether an SSE stream is consumed incrementally — and a
//! framework would sit exactly on top of the layer under test while adding
//! a dev-dependency the workspace has no other use for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_tool_mcp::{McpClient, McpError, McpServerConfig};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the stub received, as far as these tests care about it.
#[derive(Debug, Clone)]
struct Recorded {
    method: Option<String>,
    id: Option<u64>,
    headers: HashMap<String, String>,
    http_method: String,
}

struct Stub {
    url: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Stub {
    fn requests(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }

    /// Every JSON-RPC method the stub was asked for, in order.
    fn methods(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|request| request.method)
            .collect()
    }
}

/// Spawns a stub that answers each request with `responder(recorded)`.
///
/// The responder returns the complete raw HTTP response; returning `None`
/// means "accept the connection and never answer", which is what the
/// timeout test needs.
async fn spawn_stub<F>(responder: F) -> Stub
where
    F: Fn(&Recorded) -> Option<String> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let responder = Arc::new(responder);

    let task_seen = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen = task_seen.clone();
            let responder = responder.clone();
            tokio::spawn(async move {
                serve_one(stream, seen, responder).await;
            });
        }
    });

    Stub {
        url: format!("http://{addr}/mcp"),
        seen,
    }
}

async fn serve_one<F>(mut stream: TcpStream, seen: Arc<Mutex<Vec<Recorded>>>, responder: Arc<F>)
where
    F: Fn(&Recorded) -> Option<String> + Send + Sync + 'static,
{
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];

    // Read until the end of headers, then until Content-Length bytes of
    // body have arrived.
    let (head_end, content_length) = loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buffer[..read]);
        if let Some(position) = find_subslice(&raw, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..position]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            break (position + 4, length);
        }
    };

    while raw.len() < head_end + content_length {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buffer[..read]);
    }

    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();

    let mut lines = head.lines();
    let http_method = lines
        .next()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("")
        .to_owned();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();

    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let recorded = Recorded {
        method: parsed
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned),
        id: parsed.get("id").and_then(Value::as_u64),
        headers,
        http_method,
    };
    seen.lock().unwrap().push(recorded.clone());

    if let Some(response) = responder(&recorded) {
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;
    } else {
        // Hold the connection open without answering.
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn http_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn accepted() -> String {
    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
}

/// The canned `initialize` result, echoing the request's id.
fn initialize_body(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2025-06-18","serverInfo":{{"name":"stub","version":"1.0"}}}}}}"#
    )
}

fn tools_list_body(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"echo","description":"Echoes","inputSchema":{{"type":"object"}}}}]}}}}"#
    )
}

fn call_tool_body(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"text","text":"hello over http"}}]}}}}"#
    )
}

fn config(stub: &Stub) -> McpServerConfig {
    McpServerConfig::http("remote", &stub.url).request_timeout(Duration::from_secs(5))
}

// ---------------------------------------------------------------------------
// application/json reply path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_list_and_call_over_a_plain_json_endpoint() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        Some(match request.method.as_deref() {
            Some("initialize") => http_response("200 OK", "application/json", &initialize_body(id)),
            Some("tools/list") => http_response("200 OK", "application/json", &tools_list_body(id)),
            Some("tools/call") => http_response("200 OK", "application/json", &call_tool_body(id)),
            _ => accepted(),
        })
    })
    .await;

    let client = McpClient::connect(&config(&stub)).await.expect("connect");
    assert_eq!(
        client.server_info().map(|info| info.name.as_str()),
        Some("stub")
    );

    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool("echo", serde_json::json!({}))
        .await
        .expect("call tool");
    assert_eq!(result.content[0]["text"], "hello over http");

    assert_eq!(
        stub.methods(),
        vec![
            "initialize".to_string(),
            "notifications/initialized".to_string(),
            "tools/list".to_string(),
            "tools/call".to_string(),
        ]
    );
}

/// A real server sends `application/json; charset=utf-8`; matching on the
/// full header value rather than the media type would reject it.
#[tokio::test]
async fn a_content_type_with_parameters_is_still_recognized() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        Some(match request.method.as_deref() {
            Some("initialize") => http_response(
                "200 OK",
                "application/json; charset=utf-8",
                &initialize_body(id),
            ),
            _ => accepted(),
        })
    })
    .await;

    McpClient::connect(&config(&stub))
        .await
        .expect("a parameterized content type must be accepted");
}

// ---------------------------------------------------------------------------
// text/event-stream reply path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reply_delivered_over_sse_is_read_from_the_stream() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        Some(match request.method.as_deref() {
            Some("initialize") => sse_response(&[&initialize_body(id)]),
            // A progress notification ahead of the real reply: the
            // transport must skip it and keep reading, not mistake it for
            // the answer.
            Some("tools/call") => sse_response(&[
                r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}}"#,
                &call_tool_body(id),
            ]),
            _ => accepted(),
        })
    })
    .await;

    let client = McpClient::connect(&config(&stub)).await.expect("connect");
    let result = client
        .call_tool("echo", serde_json::json!({}))
        .await
        .expect("call tool over SSE");
    assert_eq!(result.content[0]["text"], "hello over http");
}

/// SSE allows one event's payload to span several `data:` lines, which the
/// receiver rejoins with `\n`.
///
/// The split point is chosen to fall *between* top-level members, where a
/// newline is legal JSON whitespace — splitting mid-token would produce
/// invalid JSON no matter how correctly the transport reassembled it, and
/// would test the JSON grammar rather than this code. Dropping the join, or
/// keeping only the last `data:` line, still fails here.
#[tokio::test]
async fn a_reply_split_across_multiple_data_lines_is_reassembled() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        if request.method.as_deref() != Some("initialize") {
            return Some(accepted());
        }
        let head = format!(r#"{{"jsonrpc":"2.0","id":{id},"#);
        let tail = r#""result":{"serverInfo":{"name":"stub","version":"1.0"}}}"#;
        Some(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: message\r\ndata: {head}\r\ndata: {tail}\r\n\r\n"
        ))
    })
    .await;

    let client = McpClient::connect(&config(&stub))
        .await
        .expect("multi-line SSE data should have been reassembled");
    assert_eq!(
        client.server_info().map(|info| info.name.as_str()),
        Some("stub"),
        "the reassembled document must be the one that was split"
    );
}

fn sse_response(events: &[&str]) -> String {
    let mut body = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for event in events {
        body.push_str("event: message\r\ndata: ");
        body.push_str(event);
        body.push_str("\r\n\r\n");
    }
    body
}

// ---------------------------------------------------------------------------
// Session id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_session_id_is_captured_at_initialize_and_echoed_afterwards() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        Some(match request.method.as_deref() {
            Some("initialize") => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: session-abc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                initialize_body(id).len(),
                initialize_body(id)
            ),
            Some("tools/list") => http_response("200 OK", "application/json", &tools_list_body(id)),
            _ => accepted(),
        })
    })
    .await;

    let client = McpClient::connect(&config(&stub)).await.expect("connect");
    client.list_tools().await.expect("list tools");

    let requests = stub.requests();
    let initialize = &requests[0];
    assert!(
        !initialize.headers.contains_key("mcp-session-id"),
        "the first request cannot carry a session id it hasn't been given yet"
    );

    let list = requests
        .iter()
        .find(|request| request.method.as_deref() == Some("tools/list"))
        .expect("tools/list was sent");
    assert_eq!(
        list.headers.get("mcp-session-id").map(String::as_str),
        Some("session-abc")
    );
    assert_eq!(
        list.headers.get("mcp-protocol-version").map(String::as_str),
        Some("2025-06-18")
    );
}

#[tokio::test]
async fn shutdown_deletes_the_session_when_one_was_issued() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        if request.http_method == "DELETE" {
            return Some(accepted());
        }
        Some(match request.method.as_deref() {
            Some("initialize") => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: session-abc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                initialize_body(id).len(),
                initialize_body(id)
            ),
            _ => accepted(),
        })
    })
    .await;

    let client = McpClient::connect(&config(&stub)).await.expect("connect");
    client.shutdown().await;

    assert!(
        stub.requests()
            .iter()
            .any(|request| request.http_method == "DELETE"),
        "shutdown should have sent a DELETE: {:?}",
        stub.requests()
    );
}

// ---------------------------------------------------------------------------
// Failure paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unexpected_content_type_is_a_typed_error_not_a_json_parse_failure() {
    let stub = spawn_stub(|_| {
        Some(http_response(
            "200 OK",
            "text/html",
            "<html>login required</html>",
        ))
    })
    .await;

    match McpClient::connect(&config(&stub)).await {
        Err(McpError::UnexpectedContentType(kind)) => assert_eq!(kind, "text/html"),
        Err(other) => panic!("expected UnexpectedContentType, got {other:?}"),
        Ok(_) => panic!("an HTML response must not be accepted as MCP"),
    }
}

#[tokio::test]
async fn a_non_success_status_carries_the_code_and_body() {
    let stub =
        spawn_stub(|_| Some(http_response("401 Unauthorized", "text/plain", "bad token"))).await;

    match McpClient::connect(&config(&stub)).await {
        Err(McpError::HttpStatus { status, body }) => {
            assert_eq!(status, 401);
            assert!(body.contains("bad token"), "{body}");
        }
        Err(other) => panic!("expected HttpStatus, got {other:?}"),
        Ok(_) => panic!("a 401 must fail the connection"),
    }
}

#[tokio::test]
async fn a_server_that_never_answers_hits_the_request_timeout() {
    let stub = spawn_stub(|_| None).await;
    let config =
        McpServerConfig::http("remote", &stub.url).request_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    match McpClient::connect(&config).await {
        Err(McpError::Timeout(_)) => {}
        Err(other) => panic!("expected Timeout, got {other:?}"),
        Ok(_) => panic!("a silent server must not appear to connect"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout should have fired promptly, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_json_rpc_error_member_surfaces_as_an_rpc_error() {
    let stub = spawn_stub(|request| {
        let id = request.id.unwrap_or(0);
        Some(match request.method.as_deref() {
            Some("initialize") => http_response("200 OK", "application/json", &initialize_body(id)),
            Some("tools/call") => http_response(
                "200 OK",
                "application/json",
                &format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32602,"message":"unknown tool"}}}}"#
                ),
            ),
            _ => accepted(),
        })
    })
    .await;

    let client = McpClient::connect(&config(&stub)).await.expect("connect");
    let error = client
        .call_tool("nope", serde_json::json!({}))
        .await
        .expect_err("an error member must surface");
    assert!(matches!(error, McpError::Rpc { code: -32602, .. }));
}

//! A minimal, real MCP server speaking JSON-RPC over stdio — used only by
//! `tests/client_e2e.rs`, spawned as a real child process the same way a
//! genuine MCP server would be. This is deliberately not a mock behind a
//! trait: `McpClient::connect` spawns a real `Command`, so the only honest
//! way to test it is against a real process on the other end of the pipe.
//!
//! Handles exactly what `crates/tools/mcp/src/client.rs` speaks: `initialize`,
//! `notifications/initialized` (silently), `tools/list` (one tool, `echo`),
//! and `tools/call` for that tool (echoes its `message` argument back) or
//! any other name (a JSON-RPC error). Two special tool-call inputs exist
//! purely to exercise client error paths: calling `echo` with
//! `{"hang": true}` never responds (tests client-side timeout), and calling
//! `crash` exits the process immediately (tests the "server closed its
//! connection" path).

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();

        // Notifications (no `id`) never get a response.
        if id.is_none() {
            continue;
        }

        let response = match method {
            "initialize" => ok(
                id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "serverInfo": { "name": "fake-mcp-server", "version": "0.0.0" },
                }),
            ),
            "tools/list" => ok(
                id,
                json!({
                    "tools": [{
                        "name": "echo",
                        "description": "Echoes its `message` argument back.",
                        "inputSchema": { "type": "object", "properties": { "message": { "type": "string" } } },
                    }],
                }),
            ),
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or("");
                let arguments = &request["params"]["arguments"];
                if name == "crash" {
                    std::process::exit(1);
                }
                if name == "echo" {
                    if arguments
                        .get("hang")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        // Never respond — exercises the client's request timeout.
                        continue;
                    }
                    let message = arguments
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": message }],
                            "isError": false,
                        }),
                    )
                } else {
                    err(id, -32602, format!("unknown tool: {name}"))
                }
            }
            other => err(id, -32601, format!("unknown method: {other}")),
        };

        let mut line = serde_json::to_string(&response).expect("serialize response");
        line.push('\n');
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.flush();
    }
}

fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Option<Value>, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

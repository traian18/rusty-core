//! End-to-end tests against a real MCP server process (`fake_mcp_server`,
//! `tests/fixtures/fake_mcp_server.rs`) — not a mock behind a trait.
//! `McpClient::connect` spawns a real `tokio::process::Command`, so the
//! only honest way to test the handshake, framing, and error paths is
//! against a real process on the other end of a real pipe.

use std::time::Duration;

use harness_tool_mcp::{connect_and_discover, McpClient, McpError, McpServerConfig};
use harness_tools::{CancellationToken, ToolExecutor, ToolInput};

fn fake_server_config(name: &str) -> McpServerConfig {
    McpServerConfig::new(name, env!("CARGO_BIN_EXE_fake_mcp_server"))
}

#[tokio::test]
async fn connect_and_discover_returns_one_namespaced_executor_per_tool() {
    let executors = connect_and_discover(&fake_server_config("fake"))
        .await
        .expect("connect and discover");

    assert_eq!(executors.len(), 1);
    let descriptor = executors[0].descriptor();
    assert_eq!(descriptor.id.as_str(), "mcp.fake.echo");
    assert_eq!(descriptor.name, "echo");
    assert!(descriptor.description.contains("Echoes"));
}

#[tokio::test]
async fn executing_the_echo_tool_round_trips_its_argument() {
    let executors = connect_and_discover(&fake_server_config("fake"))
        .await
        .expect("connect and discover");
    let echo = &executors[0];

    let result = echo
        .execute(
            ToolInput {
                arguments: serde_json::json!({ "message": "hello from the test" }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute should not error at the harness level");

    assert!(!result.is_error);
    assert_eq!(result.output["text"], "hello from the test");
}

#[tokio::test]
async fn calling_an_unknown_tool_is_a_logical_failure_not_a_harness_error() {
    // `McpClient::call_tool` talks to the server directly (bypassing the
    // `ToolExecutor` wrapper, which only ever calls tools it discovered),
    // exercising the JSON-RPC error path in `handle_line`.
    let client = McpClient::connect(&fake_server_config("fake"))
        .await
        .expect("connect");

    let error = client
        .call_tool("does-not-exist", serde_json::json!({}))
        .await
        .expect_err("unknown tool must error");

    assert!(matches!(error, McpError::Rpc { code: -32602, .. }));
}

#[tokio::test]
async fn tool_executor_reports_the_servers_rpc_error_as_a_tool_result_error() {
    // The same failure, seen through `McpToolExecutor::execute`: per its
    // own doc comment, transport/protocol failures become a logical
    // `ToolResult { is_error: true }` rather than aborting the run.
    let client = McpClient::connect(&fake_server_config("fake"))
        .await
        .expect("connect");
    let executor = harness_tool_mcp::McpToolExecutor::new(
        client,
        "fake",
        harness_tool_mcp::McpToolInfo {
            name: "does-not-exist".to_owned(),
            description: None,
            input_schema: serde_json::json!({}),
        },
    );

    let result = executor
        .execute(
            ToolInput {
                arguments: serde_json::json!({}),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute itself should not error");

    assert!(result.is_error);
}

#[tokio::test]
async fn connecting_to_a_nonexistent_command_is_a_typed_spawn_error() {
    // `McpClient` doesn't implement `Debug` (it holds non-Debug handles like
    // `Mutex<ChildStdin>`), so `Result::expect_err`/`unwrap_err` aren't
    // available on this `Result<Arc<McpClient>, McpError>` — match instead.
    match McpClient::connect(&McpServerConfig::new(
        "bad",
        "definitely-not-a-real-binary-anywhere-on-path",
    ))
    .await
    {
        Err(McpError::Spawn { .. }) => {}
        Err(other) => panic!("expected McpError::Spawn, got {other:?}"),
        Ok(_) => panic!("spawning a nonexistent binary must fail"),
    }
}

#[tokio::test]
async fn a_server_that_exits_mid_call_surfaces_as_closed_not_a_hang() {
    let client = McpClient::connect(&fake_server_config("crash-target"))
        .await
        .expect("connect");

    let error = client
        .call_tool("crash", serde_json::json!({}))
        .await
        .expect_err("a server that exits mid-call must not hang the caller");

    assert!(matches!(error, McpError::Closed));
}

#[tokio::test]
async fn cancellation_returns_promptly_instead_of_waiting_for_the_request_timeout() {
    let mut config = fake_server_config("hangs");
    config = config.request_timeout(Duration::from_secs(60)); // must not be what unblocks us
    let executors = connect_and_discover(&config)
        .await
        .expect("connect and discover");
    let echo = &executors[0];

    let cancel = CancellationToken::new();
    let cancel_after = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel_after.cancel();
    });

    let started = std::time::Instant::now();
    let error = echo
        .execute(
            ToolInput {
                arguments: serde_json::json!({ "hang": true }),
            },
            cancel,
        )
        .await
        .expect_err("a cancelled call must return Err, not a successful result");

    assert!(matches!(error, harness_tools::ToolError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation should short-circuit well before the 60s request timeout, took {:?}",
        started.elapsed()
    );
}

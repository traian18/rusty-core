# rusty-transport-mcp

MCP **server** mode: exposes a running harness as an MCP server so Claude Desktop, Cursor or
VS Code can drive real sessions with no harness-specific SDK. It is the mirror of `rusty-tool-mcp`,
which consumes other MCP servers; together they cover both directions.

## Why it exists

MCP clients are everywhere, and each already knows how to call a tool. Offering the harness as an
MCP server lets them start and drive sessions without learning a new protocol. Like the other
transports it sits on `RpcHandler`, so sessions created over MCP flow through the same typed
contract, admission and revision tracking as every other client.

## How it works

`serve(config, handler, shutdown)` (or `serve_io` for any reader and writer) runs an ND-JSON
JSON-RPC loop implementing `initialize`, `tools/list`, `tools/call`, `resources/list` and
`resources/read`. The tool surface (`tools`) maps MCP tool calls onto `RpcRequestBody`s: create a
session, send a prompt, wait for the result (bounded by the prompt timeout), and read sessions as
resources. Sampling, roots and prompts are not implemented.

## Key types and concepts

- **`McpServeConfig`**: what an MCP client cannot say: the integration id and its configuration,
  the workspace root (fixed here so a connected IDE cannot point the agent at an arbitrary
  directory), the toolset, skills, and the prompt timeout (default 10 minutes).
- **`RpcHandler`**: the same contract the other transports serve.
- **Stdout ownership**: as with the stdio transport, stdout is the protocol.

## A worked example

```json
{ "integration": "anthropic", "workspace_root": "/work/shop" }
```

A client then lists tools and calls one that starts a session in `/work/shop` with the configured
integration; the final answer comes back as the tool result.

## Public API

- `serve`, `serve_io`, `McpServeConfig` and its builder methods (`integration_config`, `toolset`,
  `skills`, `prompt_timeout`).

## Errors

JSON-RPC errors with MCP's codes for unknown methods and bad parameters; a prompt that exceeds the
timeout returns a tool error rather than hanging the client.

## Design decisions

- **The workspace is configuration, not an argument.** A tool call cannot choose where the agent
  works.
- **Only the needed MCP methods.** Unimplemented capabilities are absent rather than stubbed.

## Testing

Unit tests sit beside `run.rs`, `tools.rs` and `lib.rs`; `tests/server_e2e.rs` drives the server end to end. Run
`cargo test -p rusty-transport-mcp`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `harnessd`

## Must never

- Know what a request means: that is the `RpcHandler` the daemon supplies.
- Depend on the engine or on session management.
- Let a tool call choose the workspace root.
- Write anything to stdout except protocol lines.

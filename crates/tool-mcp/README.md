# rusty-tool-mcp

The MCP **client**. It connects to external Model Context Protocol servers (filesystem, GitHub,
Slack, a database, ...) and exposes every tool they advertise as an ordinary `ToolExecutor`, so an
MCP tool is called exactly like a built-in one. The mirror image, exposing the harness *as* an MCP
server, is `rusty-transport-mcp`.

## Why it exists

MCP is how the wider ecosystem ships tools. Supporting it lets an agent use any MCP server without
a new crate per server. Adapting each remote tool into `ToolExecutor` means permissions,
cancellation and the event stream apply to MCP tools with no special cases.

## How it works

1. `McpServerConfig` names a server and how to reach it: a command to spawn (stdio) or a URL
   (streamable HTTP), with environment, working directory and a request timeout.
2. The client starts the transport (`transport/stdio.rs` spawns the process and runs a reader task
   that matches responses to pending requests; `transport/http.rs` posts requests), performs the
   MCP handshake, and lists the server's tools.
3. Each tool becomes an `McpToolExecutor` registered as `mcp.<server>.<tool>`, so two servers that
   both offer `search` never collide.
4. A call sends `tools/call`; the result's content is returned as the tool result, and a
   server-reported error becomes an error result.

## Key types and concepts

- **`McpServerConfig`, `McpTransportConfig`**: how to reach a server.
- **`McpClient`**: the connection; owns the request/response matching.
- **`McpToolExecutor`**: the adapter from one remote tool to `ToolExecutor`.
- **Namespacing**: `mcp.<server>.<tool>`.

## A worked example

```json
{ "name": "github", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] }
```

registers tools such as `mcp.github.create_issue`. When the model calls it, the arguments are
forwarded as MCP `tools/call`, and the server's text content comes back as the result.

## Public API

- `McpServerConfig`, `McpTransportConfig`, `McpClient`, `McpToolExecutor`, `McpError`, and the
  protocol types in `protocol`.

## Errors

`McpError` covers spawn failures (with the server name and the OS error), transport failures,
protocol errors, timeouts and a server that closed the connection. A tool-level error from the
server is returned as an error result, not an `McpError`.

## Design decisions

- **Tools are namespaced.** Collisions between servers are impossible by construction.
- **The child process dies with the client** (`kill_on_drop`).
- **Missing pipes are errors**, not panics: a spawned process whose stdio handles are absent
  fails with a spawn error naming the stream.

## Testing

`tests/client_e2e.rs` runs the client against a fake MCP server binary (`fake_mcp_server`),
covering the handshake, tool listing, calls, errors and timeouts; `tests/http_transport.rs`
covers the HTTP transport. Run `cargo test -p rusty-tool-mcp`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-tool-api`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
- Trust a server's output as instructions; it is data the model reads.

# harness-tool-mcp

The MCP **client**. It connects to external Model Context Protocol servers (filesystem, GitHub, Slack, a database, …) and exposes every tool they advertise as an ordinary `ToolExecutor`, so an MCP tool is called exactly like a built-in one.

## What's inside

- `client`, `protocol` — the MCP client and wire types.
- `transport` — stdio (spawn a process) and streamable HTTP.
- `config` — `McpServerConfig`; tools are namespaced `mcp.<server>.<tool>` so servers never collide.
- `tool` — `McpToolExecutor`, the adapter into `harness-tools`.

The mirror image, exposing the harness *as* an MCP server, is [`harness-transport-mcp`](../../transports/mcp).

## In the workspace

- **Depends on:** `harness-protocol`, `harness-tools`
- **Used by:** `harness-engine`

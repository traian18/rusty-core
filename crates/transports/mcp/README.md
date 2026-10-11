# harness-transport-mcp

MCP **server** mode: exposes a running harness as an MCP server so Claude Desktop, Cursor or VS Code can drive real sessions with no harness-specific SDK. It is the mirror of `harness-tool-mcp`, which consumes other MCP servers; together they cover both directions.

## What's inside

- `tools`, `wire`, `run` — the MCP tool surface, wire types and the serve loop.
- `McpServeConfig` — the integration and workspace root sessions use, since an MCP client cannot know them.

Like the other transports it sits on `RpcHandler`, so it needs no knowledge of the engine.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-runtime`
- **Used by:** `harnessd`

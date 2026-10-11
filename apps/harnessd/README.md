# harnessd

The harness as a daemon. It builds a `Harness` with every integration registered and exposes it over one or more transports, so an external process (an IDE, a script, another language) can drive sessions without linking Rust.

## What's inside

- Transports (at least one is required, any combination): `--unix-socket <path>`, `--tcp <addr>` (WebSocket, loopback only), `--stdio`.
- `--mcp-stdio` — serve as an MCP server instead (mutually exclusive with `--stdio`), with `--mcp-integration`, `--mcp-integration-config` and `--mcp-workspace-root`.
- Sessions are persisted with the JSONL store, by default under `<cwd>/.harness/sessions`.
- `HarnessRpcHandler` — maps the RPC protocol onto `Harness` and `SessionManager`.

Logging goes to stderr unconditionally so the stdio transport's stdout stays a clean RPC stream. Use [`harnessctl`](../harnessctl) to talk to it.

## In the workspace

- **Depends on:** `harness-engine`, `harness-integration-anthropic`, `harness-integration-codex`, `harness-integration-gemini`, `harness-integration-github-copilot`, `harness-integration-openai`, `harness-integration-openai-compatible`, `harness-protocol`, `harness-runtime`, `harness-session-store`, `harness-transport-ipc`, `harness-transport-mcp`, `harness-transport-stdio`, `harness-transport-websocket`
- **Used by:** applications and external embedders (a leaf of the workspace)

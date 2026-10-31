# harnessd

The harness as a daemon. It builds a `Harness` with every integration registered and serves it over
one or more transports, so an external process (an IDE, a script, another language) can drive sessions
without linking Rust.

## Why it exists

Not every client can link the engine, and some should not (a sandbox boundary, a different
language, a long-lived process shared by several clients). The daemon is the engine behind the wire
protocol: one process owns the sessions, and any number of clients speak `rusty-protocol` to it.

## How it works

1. `main` parses the transports and builds the `Harness`: all provider integrations, the JSONL
   session store (by default `<cwd>/.harness/sessions`), logging to stderr.
2. It starts the chosen transports (`--unix-socket`, `--tcp` as loopback WebSocket, `--stdio`), or
   serves as an MCP server (`--mcp-stdio`) instead.
3. `HarnessRpcHandler` implements `RpcHandler`: it maps each request (hello, create or restore
   session, mutation, subscribe, snapshot, diagnostics) to the `Harness` and `SessionManager`.
4. The transports own the handshake and subscription replay; the handler never sees a request before
   `Hello`.

## Key types and concepts

- **Transport**: how bytes arrive (Unix socket, WebSocket, stdio, MCP).
- **`HarnessRpcHandler`**: the meaning of each request.
- **Stdout discipline**: logs go to stderr so the stdio transport's stdout stays a clean stream.

## A worked example

```console
harnessd --unix-socket /tmp/harness.sock
harnessctl --socket /tmp/harness.sock chat
```

## Public API

A binary. At least one transport is required; `--mcp-stdio` is mutually exclusive with `--stdio` and
takes `--mcp-integration`, `--mcp-integration-config` and `--mcp-workspace-root`.

## Errors

Startup fails with a message when no transport is given or a socket cannot be bound. Per-request
errors are `Failure` responses with a category and code, never a dropped connection.

## Design decisions

- **Loopback only** for TCP: there is no authentication in the protocol.
- **One handler for every transport**, so behavior is identical across them.

## Testing

`tests/conformance.rs` replays the protocol contract and `tests/end_to_end.rs` drives real sessions
over a socket with a fake backend. Run `cargo test -p harnessd`.

## In the workspace

- **Depends on:** `rusty-engine`, `rusty-protocol`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-gemini`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-provider-openai-compatible`, `rusty-runtime`, `rusty-session-store`, `rusty-transport-ipc`, `rusty-transport-mcp`, `rusty-transport-stdio`, `rusty-transport-websocket`
- **Used by:** nothing else in the workspace

## Must never

- Write anything but protocol frames to stdout in stdio mode.
- Listen on a non-loopback address.
- Answer a request before the connection has said `Hello`.

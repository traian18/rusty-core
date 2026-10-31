# rusty-transport-stdio

The same `RpcHandler` contract over stdin and stdout, using newline-delimited JSON: one request
or response per line. Suited to an IDE that spawns the daemon as a child process.

## Why it exists

The daemon must be reachable by an IDE over a socket, by a browser over WebSocket, by a parent
process over stdio, and by MCP clients. How bytes are framed differs for each; what a request
*means* does not. So each transport crate only moves framed requests and responses, and the
conversation itself (the `Hello` handshake, subscriptions that replay missed events and then
stream live ones, dispatch to the `RpcHandler`) lives once in `rusty_runtime::rpc::RpcConnection`.
The transport crates never depend on the engine.

## How it works

`serve(handler, shutdown)` serves the process's real stdin and stdout. `serve_io(reader, writer,
handler, shutdown)` does the same for any reader and writer, which is how tests drive it over an
in-memory pipe. A writer task writes each queued line plus a newline and flushes; the read loop
reads lines, skips blank ones, decodes each into an `RpcRequest`, and hands it to an
`RpcConnection`. Clean end of input ends the connection.

## Key types and concepts

- **ND-JSON framing**: one JSON value per line; simpler than header-framed protocols because
  nothing here needs binary-safe payloads.
- **Stdout ownership**: once serving, stdout belongs to the protocol.
- **`FrameSink`**: queues serialized lines for the writer task.

## A worked example

```text
> {"id":1,"session_id":null,"body":{"type":"hello","payload":{"protocol_version":2}}}
< {"id":1,"body":{"type":"hello","payload":{"protocol_version":2,"capabilities":{..}}}}
```

A stray `println!` anywhere in the process would put a non-protocol line on stdout and corrupt
the stream, which is why `harnessd` sends all logging to stderr before serving.

## Public API

- `serve(handler, shutdown)` and `serve_io(reader, writer, handler, shutdown)`.

## Errors

`io::Error` from reading or writing. Malformed lines are answered with
`protocol.invalid_request`; protocol violations are `Failure` responses, as in the other
transports.

## Design decisions

- **Stdout is reserved.** The crate documents the rule rather than trying to enforce it; the
  daemon routes logging to stderr.
- **`serve_io` exists for tests**, so the real stdio pair is never needed to test the protocol.

## Testing

`src/tests.rs` runs the protocol over an in-memory duplex pipe. Run
`cargo test -p rusty-transport-stdio`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `harnessd`

## Must never

- Know what a request means: that is the `RpcHandler` the daemon supplies.
- Depend on the engine or on session management.
- Answer a request before the connection has said `Hello`.
- Write anything to stdout except protocol lines.

# rusty-transport-ipc

Exposes an `RpcHandler` over a Unix domain socket, using length-prefixed JSON frames. It moves
framed `RpcRequest` and `RpcResponse` bytes and nothing more; `harnessd` implements the handler and
is the only thing that knows what a request means. Access is gated by filesystem permissions on
the socket.

## Why it exists

The daemon must be reachable by an IDE over a socket, by a browser over WebSocket, by a parent
process over stdio, and by MCP clients. How bytes are framed differs for each; what a request
*means* does not. So each transport crate only moves framed requests and responses, and the
conversation itself (the `Hello` handshake, subscriptions that replay missed events and then
stream live ones, dispatch to the `RpcHandler`) lives once in `rusty_runtime::rpc::RpcConnection`.
The transport crates never depend on the engine.

## How it works

1. `serve(socket_path, handler, shutdown)` removes a stale socket file left by a crash (a leftover
   path makes `bind` fail even though nothing listens), binds, and accepts connections until
   `shutdown` fires.
2. Each connection gets a writer task that drains an outgoing queue into frames, and a read loop
   that reads one frame at a time (`framing::read_frame`), decodes it into an `RpcRequest`, and
   hands it to an `RpcConnection`.
3. `RpcConnection::accept` applies the protocol (handshake first, then dispatch). Its answers go
   through a `FrameSink`, which serializes each `RpcResponse` into a frame and queues it.
4. A frame that does not decode is answered with a `protocol.invalid_request` failure and the
   connection stays open.

## Key types and concepts

- **Frame**: a length prefix followed by that many bytes of JSON (`framing`).
- **`FrameSink`**: this transport's `ResponseSink`; it frames responses for the writer task.
- **`RpcConnection`**: the per-connection protocol state, shared with the other transports.

## A worked example

```text
client -> {"id":1,"session_id":null,"body":{"type":"hello","payload":{"protocol_version":2}}}        (one frame)
daemon -> {"id":1,"body":{"type":"hello","payload":{"protocol_version":2,"capabilities":{..}}}}
client -> {"id":2,"session_id":"...","body":{"type":"subscribe","payload":{"since_seq":41}}}
daemon -> {"id":2,"body":{"type":"ack"}}  then events 42, 43, ... as they happen
```

A first request that is not `Hello` is answered with `protocol.hello_required`, and a version
mismatch with `protocol.version_mismatch`.

## Public API

- `serve(socket_path, handler, shutdown)`, and the `framing` module (`read_frame`, `write_frame`),
  which clients such as `harnessctl` also use.

## Errors

`serve` returns an `io::Error` for bind or accept failures. Protocol problems are not errors of
the function: they are `Failure` responses to the client (`protocol.invalid_request`,
`protocol.hello_required`, `protocol.version_mismatch`, `request.missing_session_id`).

## Design decisions

- **Local only.** A Unix socket relies on filesystem permissions rather than its own
  authentication.
- **Responses that fail to serialize are logged and dropped**, not panicked on, so one bad value
  cannot take down a connection.

## Testing

`src/tests.rs` drives a real socket with a fake handler: handshake, ordering, subscription replay
and live events, invalid frames, and shutdown. Run `cargo test -p rusty-transport-ipc`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `harnessctl`, `harnessd`

## Must never

- Know what a request means: that is the `RpcHandler` the daemon supplies.
- Depend on the engine or on session management.
- Answer a request before the connection has said `Hello`.

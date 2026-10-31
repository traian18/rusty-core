# rusty-transport-websocket

The same `RpcHandler` contract over TCP and WebSocket: each text message carries one JSON
`RpcRequest` or `RpcResponse`, so no extra length prefix is needed. **Security:** a TCP port is not
protected by filesystem permissions and this version has no authentication; bind loopback
(`127.0.0.1`) only.

## Why it exists

The daemon must be reachable by an IDE over a socket, by a browser over WebSocket, by a parent
process over stdio, and by MCP clients. How bytes are framed differs for each; what a request
*means* does not. So each transport crate only moves framed requests and responses, and the
conversation itself (the `Hello` handshake, subscriptions that replay missed events and then
stream live ones, dispatch to the `RpcHandler`) lives once in `rusty_runtime::rpc::RpcConnection`.
The transport crates never depend on the engine.

WebSocket is how a browser or a remote tool reaches the daemon; it frames messages already, which
makes this the thinnest of the transports.

## How it works

`serve(addr, handler, shutdown)` binds and calls `serve_listener`, which accepts TCP connections
until `shutdown` fires; tests call `serve_listener` with a listener bound to port 0 so the OS
picks a free port. Each connection is upgraded with `accept_async`, split into a read half and a
write half, and run like the other transports: a writer task sends queued text messages, and the
read loop decodes each text message into an `RpcRequest` for an `RpcConnection`. Non-text
messages are ignored.

## Key types and concepts

- **Text message**: one JSON value; WebSocket supplies the framing.
- **`serve_listener`**: serves an already-bound listener, so callers can learn the bound address.
- **`FrameSink`**: queues serialized text for the writer task.

## A worked example

```text
ws://127.0.0.1:7421   (one text message per line below)
> {"id":1,"session_id":null,"body":{"type":"hello","payload":{"protocol_version":2}}}
< {"id":1,"body":{"type":"hello","payload":{"protocol_version":2,"capabilities":{..}}}}
```

## Public API

- `serve(addr, handler, shutdown)` and `serve_listener(listener, handler, shutdown)`.

## Errors

Bind and accept failures are `io::Error`; a failed WebSocket handshake ends that connection
with a logged warning. Protocol problems are `Failure` responses.

## Design decisions

- **Unauthenticated by design in this version.** Binding anything but loopback needs an
  authentication check (for example a bearer token verified during the handshake) added at the
  call site first.

## Testing

`src/tests.rs` connects real WebSocket clients to a listener on an ephemeral loopback port. Run
`cargo test -p rusty-transport-websocket`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `harnessd`

## Must never

- Know what a request means: that is the `RpcHandler` the daemon supplies.
- Depend on the engine or on session management.
- Answer a request before the connection has said `Hello`.
- Be bound to a non-loopback address without authentication added first.

# harness-transport-ipc

Exposes an `RpcHandler` over a Unix domain socket, using length-prefixed JSON frames. It only moves framed `RpcRequest`/`RpcResponse` bytes and dispatches them; it knows nothing about `Harness` or sessions. `harnessd` implements the handler and is the only thing that knows what a request means.

## What's inside

- `framing` — the length-prefixed frame codec.
- Server and client halves, with the mandatory `Hello` protocol-version handshake on every connection.

Access is gated by filesystem permissions on the socket.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-runtime`
- **Used by:** `harnessctl`, `harnessd`

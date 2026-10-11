# harness-transport-websocket

The same `RpcHandler` contract as the IPC transport over TCP + WebSocket: each text message carries one JSON `RpcRequest` or `RpcResponse`, so no extra length prefix is needed.

## What's inside

- Server that accepts WebSocket connections and dispatches to an `RpcHandler`, with the `Hello` handshake.

**Security:** a TCP port is not protected by filesystem permissions and this version is unauthenticated. Bind to loopback (`127.0.0.1`) only; see the crate docs before binding anything else.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-runtime`
- **Used by:** `harnessd`

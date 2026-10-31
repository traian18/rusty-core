# PLAN: `harness-transport-websocket`

## Why this exists

Same motivation as `harness-transport-ipc` (see that crate's `PLAN.md` for the full rationale and
the shared `RpcRequest`/`RpcResponse`/`RpcHandler` contract in `harness-protocol::rpc` — read that
first, this file only covers what's specific to websocket). WebSocket is the transport for
"IDE runs the harness remotely, or in a container/VM, or the IDE's extension host is a browser
context (e.g. a web-based IDE, or a VS Code web extension)" — anywhere a Unix socket isn't
reachable but a TCP port is.

Currently a 3-line stub with no wire library.

## Design

Reuses the exact `RpcRequest` / `RpcResponse` / `RpcHandler` types from `harness-protocol::rpc`
(do not redefine them here). The only difference from the IPC crate is the framing and transport:

- Each WebSocket **message** carries exactly one `serde_json`-encoded `RpcRequest` or
  `RpcResponse` — no extra length-prefixing needed, WebSocket already frames messages. Use text
  frames (simplest to debug with browser devtools / `wscat`) rather than binary.
- Bind with `tokio::net::TcpListener`, perform the WS upgrade with `tokio-tungstenite` (new
  dependency — not in the workspace yet).
- Same dispatch shape as IPC: one task per connection reads frames and calls
  `handler.handle(...)`, a `Subscribe` request spins up an mpsc-owned sender task that pushes
  `Event(...)` frames.

```rust
pub async fn serve(
    addr: std::net::SocketAddr,
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()>
```

## Cargo.toml additions

```toml
[dependencies]
harness-protocol = { path = "../../harness-protocol" }
tokio = { workspace = true, features = ["net", "rt", "io-util", "sync", "macros"] }
tokio-tungstenite = "0.24"
futures = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
```
`tokio-tungstenite` needs to be added to `[workspace.dependencies]` in the root `Cargo.toml` —
it isn't there yet.

## Implementation steps

1. Do this *after* `harness-transport-ipc` lands the `harness-protocol::rpc` module — no need to
   duplicate that design work.
2. Add `tokio-tungstenite` to the workspace.
3. Implement `serve()`: TCP accept loop → WS upgrade (`tokio_tungstenite::accept_async`) → split
   into sink/stream → same dispatch loop as the IPC crate (the connection-handling logic is
   structurally identical enough that it's worth asking, once both exist, whether to factor the
   shared "dispatch a stream of RpcRequest against an RpcHandler" loop into a small helper — but
   don't build that abstraction speculatively before both concrete implementations exist).
4. Integration test using `tokio-tungstenite`'s client half against a local `serve()` instance.

## Security note

Unlike the IPC socket (gated by filesystem permissions), a TCP listener is reachable by anything
that can route to that address. If this is ever bound to something other than `127.0.0.1`, it
needs auth (a bearer token in the WS upgrade request, checked before completing the handshake) —
flag this explicitly in `harnessd`'s config rather than silently defaulting to an open port. v1
scope here is "loopback-only, no auth" with a loud comment/doc saying so; don't build a token
scheme speculatively until there's an actual remote-access use case.

## Non-goals for v1

- TLS termination (terminate TLS in front of this with a reverse proxy if remote access is ever
  needed; don't build it into the crate).
- Auth (see above).

# PLAN: `rusty-transport-ipc`

## Why this exists

Right now the only way to drive a session is in-process: `apps/harness/src/harness_setup.rs`
calls `Harness::builder()...build()` and `harness.session()...start()` in the same binary as the
TUI. There is no way for an external process — an IDE — to talk to a harness. This crate is the
first of three transports that make the harness reachable over a wire. IPC (a Unix domain socket)
is the right default for "IDE extension talks to a local harness process on the same machine" —
lower overhead than TCP/websocket, no port allocation, filesystem permissions gate access.

This crate is empty today (3-line stub). It also defines the wire contract shared by all three
transports (`websocket` and `stdio` reference this plan instead of repeating it).

## Shared contract (defined once, used by all three transport crates)

Add a new module `rusty-protocol::rpc` (`crates/protocol/src/rpc.rs`) with the
request/response envelope every transport speaks. This is the natural home since
`rusty-protocol` is already the crate every other piece depends on for wire types, and its
types (`AgentEventEnvelope`, `SessionCommand`-equivalents, etc.) are already `Serialize`/
`Deserialize`.

```rust
// rusty-protocol/src/rpc.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: RequestCorrelationId,       // client-assigned, echoed back in RpcResponse
    pub session_id: Option<SessionId>,  // None only for CreateSession
    pub body: RpcRequestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcRequestBody {
    CreateSession {
        workspace_root: PathBuf,
        integration: String,             // e.g. "anthropic"
        integration_config: serde_json::Value,
        toolset: AgentToolset,
    },
    Prompt(UserInput),
    Cancel,
    Pause,
    Resume,
    ResolvePermission { id: PermissionId, decision: PermissionDecision },
    Snapshot,
    Subscribe,     // start streaming AgentEventEnvelope for this session_id on this connection
    CloseSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: RequestCorrelationId,        // correlates to the request, or absent for pushed events
    pub body: RpcResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcResponseBody {
    SessionCreated { session_id: SessionId },
    Ack,
    Snapshot(SessionSnapshotWire),       // serializable projection of SessionSnapshot
    Event(AgentEventEnvelope),           // pushed, unsolicited, tagged with session_id
    Error { message: String },
}
```

Every request carries an explicit `session_id` (bake in multi-session support from the start —
an IDE will have one session per open chat/tab through a single daemon connection, and retrofitting
this later would mean re-doing the framing). `Subscribe` turns the connection into a
push-consumer for that session's event stream in addition to still accepting requests
(full-duplex) — a single TCP/socket connection both sends commands and receives the live event
feed, matching how `SessionClient::subscribe()` already works today.

Also define the handler trait that decouples "how bytes move" (this crate) from
"what a request means" (`apps/harnessd`):

```rust
// rusty-protocol/src/rpc.rs
#[async_trait]
pub trait RpcHandler: Send + Sync {
    async fn handle(&self, session_id: Option<SessionId>, body: RpcRequestBody) -> RpcResponseBody;
    fn subscribe(&self, session_id: SessionId) -> Option<broadcast::Receiver<AgentEventEnvelope>>;
}
```

`harnessd` implements `RpcHandler` by wrapping `Harness`/`SessionManager` (see
`apps/harnessd/PLAN.md`). This crate only ever depends on `rusty-protocol` for these types —
never on `harness-engine` or `harness-runtime` directly — so it stays a thin framing layer.

## This crate's job

1. **Framing.** Length-prefixed JSON over a `tokio::net::UnixListener`/`UnixStream`: 4-byte
   little-endian `u32` length, then that many bytes of `serde_json`-encoded `RpcRequest` /
   `RpcResponse`. Simple, symmetric, no external framing crate needed.
2. **Connection loop.** Per accepted connection: spawn a task that reads frames, dispatches each
   to `handler.handle(...)`, writes the response frame back. When a `Subscribe` request comes in,
   spawn a second task that drains `handler.subscribe(session_id)` and writes `Event(...)` frames
   to the same socket (needs a `tokio::sync::Mutex` around the write half, or an mpsc fan-in task
   that owns the write half exclusively — prefer the mpsc approach to avoid write interleaving
   corrupting frames).
3. **Public API:**
   ```rust
   pub async fn serve(socket_path: &Path, handler: Arc<dyn RpcHandler>) -> std::io::Result<()>
   ```
   Binds, removes a stale socket file if present (Unix sockets don't clean up after a crash),
   loops accepting connections, each on its own task. Returns only on a fatal bind error;
   shutdown is caller-driven (wrap the accept loop in a `tokio::select!` against a
   `CancellationToken` passed in, so `harnessd` can stop it on SIGTERM).

## Cargo.toml additions

```toml
[dependencies]
rusty-protocol = { path = "../protocol" }
tokio = { workspace = true, features = ["net", "rt", "io-util", "sync", "macros"] }
serde_json = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
```
No new external crates needed — `tokio::net::UnixListener` covers this without pulling in
`interprocess` or similar. Note: Unix domain sockets are macOS/Linux only; Windows named-pipe
support is explicitly out of scope for v1 (flag it as a follow-up if Windows IDE support matters
later, don't build it speculatively now).

## Implementation steps

1. Add `rpc.rs` to `rusty-protocol` (shared with websocket/stdio — do this once, first).
2. Implement frame read/write helpers (`read_frame`, `write_frame`) — small, testable in
   isolation with an in-memory duplex (`tokio::io::duplex`).
3. Implement `serve()` with the accept loop + per-connection dispatch task.
4. Implement the `Subscribe` fan-in (mpsc-owned write half).
5. Integration test: spin up `serve()` against a `FakeRpcHandler` (mirrors `FakeBackend` /
   `FakeToolRegistry` patterns already used in `harness-runtime/src/testing/`), connect a raw
   `UnixStream` client, assert request/response round-trip and event delivery ordering.

## Non-goals for v1

- Authentication/authorization on the socket (rely on filesystem permissions on the socket path
  for now — note this explicitly to whoever wires up `harnessd`, since a world-readable socket
  path would let any local user drive the harness).
- Windows named pipes.
- Multiplexing multiple *concurrent* subscriptions per connection beyond one `Subscribe` per
  `session_id` (a client that wants N sessions streaming opens N connections, or the plan can be
  revisited once a real IDE integration surfaces the need).

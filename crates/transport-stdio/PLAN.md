# PLAN: `harness-transport-stdio`

## Why this exists

Same shared contract as `harness-transport-ipc` (see that crate's `PLAN.md` for the full
`RpcRequest`/`RpcResponse`/`RpcHandler` design — read that first). Stdio is the transport for
"IDE spawns `harnessd` (or a single-session harness binary) as a child process and talks to it
over its stdin/stdout pipes" — this is the same integration shape as the Language Server Protocol,
and it's the easiest one for an IDE extension to adopt first (`child_process.spawn` + pipe, no
socket path or port to manage, no permissions to reason about).

Currently a 3-line stub.

## Design

Frame each `RpcRequest`/`RpcResponse` as one line of `serde_json`-encoded newline-delimited JSON
(ND-JSON) on stdin/stdout respectively. This is deliberately simpler than LSP's
`Content-Length:`-header framing — ND-JSON is trivial to produce/consume from any language without
a header parser, and nothing here requires binary-safe payloads. If an IDE integration later
specifically wants LSP-style framing for consistency with existing LSP client libraries, that's a
small, isolated change to the framing function — don't build both up front.

```rust
pub async fn serve(
    handler: Arc<dyn RpcHandler>,
    shutdown: CancellationToken,
) -> std::io::Result<()>
```

Reads `RpcRequest` lines from `tokio::io::stdin()`, dispatches to `handler.handle(...)`, writes
`RpcResponse` lines to `tokio::io::stdout()`. Because stdio is a single fixed pipe pair (not a
listener accepting many connections), there's exactly one logical "connection" for the process
lifetime — `Subscribe` still works the same way as the other transports (an mpsc-owned writer task
interleaves response frames and pushed `Event(...)` frames onto stdout), it just doesn't need an
accept loop.

**Important constraint:** stdout must be reserved exclusively for the RPC stream once this is
serving. Nothing else in the process (a stray `println!`, a panic hook writing to stdout, a
dependency's default logger) can share it, or frames get corrupted. Route `tracing` output to
stderr (`tracing_subscriber::fmt().with_writer(std::io::stderr)`) wherever this transport is used
— call this out loudly in `apps/harnessd/PLAN.md` too, since that's where it'll bite someone.

## Cargo.toml additions

```toml
[dependencies]
harness-protocol = { path = "../../harness-protocol" }
tokio = { workspace = true, features = ["io-std", "rt", "io-util", "sync", "macros"] }
serde_json = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
```

## Implementation steps

1. Do this after (or alongside) `harness-transport-ipc` — depends only on the shared
   `harness-protocol::rpc` module, not on the IPC crate's code.
2. Implement line-delimited read loop over `tokio::io::BufReader<Stdin>::lines()` (or manual
   `read_until(b'\n', ...)` if a request could theoretically contain embedded newlines in a JSON
   string escaped correctly — `serde_json`'s single-line `to_string()` output never contains a raw
   newline, so `.lines()` is safe here).
3. Implement the mpsc-owned stdout writer task (same shape as the other two transports).
4. Test by feeding a `tokio::io::duplex()` pair standing in for stdin/stdout against a
   `FakeRpcHandler`, rather than spawning a real subprocess in the test — keeps it fast and
   deterministic.

## Non-goals for v1

- Reconnection / multiple stdio sessions (a stdio transport dies with its process — that's the
  point; the IDE respawns if needed).
- Any non-JSON framing variant (LSP `Content-Length` headers) — add only if a concrete client
  needs it.

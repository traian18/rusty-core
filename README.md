# Rust Agent Harness

Rust Agent Harness is a reusable, embeddable runtime for building **tool-using AI agents**. It separates deterministic agent and session semantics from asynchronous execution, model providers, tools, persistence, transports, and user interfaces — so the same session behavior can run in a terminal, a daemon, an IDE, a TUI, or any other host application.

The repository is a Cargo workspace of small crates with explicit responsibility boundaries: a pure protocol/type layer, an async runtime, a public session-builder API, pluggable model backends, pluggable tools, durable persistence, and three wire transports that expose all of it to processes that aren't Rust at all.

This README covers what the engine does, how it works, and the two ways to integrate it: **embedding the crates in a Rust application**, or **talking to a `harnessd` daemon over a socket/stdio/WebSocket**.

---

## Capabilities at a glance

- **Deterministic agent state machine** — `harness-core` models an `Agent` as pure state with a single `apply()` transition function. Same inputs, same transitions, regardless of timing or transport; the async runtime only *executes* what the state machine decides.
- **Multi-session, multi-agent runtime** — `harness-runtime` runs any number of sessions concurrently (each with a root agent that can spawn child agents), with a shared scheduler, resource manager, and per-backend rate limiting.
- **Streaming event model** — every observable occurrence is an `AgentEventEnvelope` carrying routing metadata (`session_id`, `agent_id`, `parent_agent_id`, `run_id`) and two monotonic sequence numbers for exact ordering. Subscribers get a live push stream; reconnecting clients can **resume from a sequence number without gaps or duplicates**.
- **Durable session persistence** — every durable event is written to a `SessionStore` (JSONL or WAL-mode SQLite) as it happens, plus periodic state snapshots. Sessions survive daemon restarts and can be restored via `Harness::restore_session`. Raw streaming deltas stay ephemeral by design (see [Durability](#durability-and-resume)).
- **Pluggable model backends** — Anthropic Messages API, OpenAI Chat Completions, any OpenAI-compatible endpoint (OpenRouter, Ollama, vLLM, …), Gemini, or the `claude`/`codex` CLIs driven as subprocesses. All share one provider-neutral backend adapter.
- **Built-in resilience** — every model call goes through retry with exponential backoff + jitter, a shared deadline across attempts, and a circuit breaker. Settings are configurable per provider (see [Provider resilience](#provider-resilience)).
- **Pluggable tools** — filesystem read/edit/search, shell execution, read-only git, and web fetch ship out of the box; `harness-extension-api` is the stable surface for writing your own tools and backends.
- **Permission gating** — tool calls can be configured `Allow` / `Ask` / (deny); pending requests surface as events and are resolved per-call (`y`/`n` in the TUIs, `ResolvePermission` on the wire).
- **Hierarchical cancellation** — a root `CancellationToken` fans out to every session, agent, backend request, and tool call; cancelling anywhere propagates and is idempotent.
- **Three wire transports, one RPC contract** — Unix domain socket (length-prefixed JSON), WebSocket, and stdio (newline-delimited JSON) all frame the same `RpcRequestBody`/`RpcResponseBody` types, with a mandatory `Hello` protocol-version handshake on every connection.
- **Works in-process or out-of-process** — the same engine is embedded directly by `apps/harness`'s TUI and exposed by `apps/harnessd` for external clients like `apps/harnessctl`.

---

## How it works

### Layered architecture

The workspace is split into five layers, each depending only on the ones below it:

```
┌────────────────────────────────────────────────────────────────┐
│ Apps          harness (TUI) · harnessd (daemon) · harnessctl   │
│               (CLI + chat TUI)                                  │
├────────────────────────────────────────────────────────────────┤
│ Transports    ipc · websocket · stdio                          │
│               (one RPC contract: harness_protocol::rpc)        │
├────────────────────────────────────────────────────────────────┤
│ Integrations  anthropic · openai · openai-compatible · gemini  │
│               · claude-code · codex                            │
│ Tools         filesystem · shell · git · web                   │
├────────────────────────────────────────────────────────────────┤
│ Engine        harness-engine (public Harness/SessionBuilder)   │
│               harness-runtime (async orchestration)            │
│               harness-core (deterministic agent state machine) │
├────────────────────────────────────────────────────────────────┤
│ Protocol      harness-protocol (pure types, no I/O)            │
│               harness-model · harness-generic-backend          │
│               harness-context · harness-workspace              │
│               harness-session-store · harness-extension-api    │
└────────────────────────────────────────────────────────────────┘
```

- **`harness-protocol`** — pure serializable types only (no runtime, no I/O policy): requests/responses, events, commands, ids, usage, tools. This is the contract every transport and every wire client speaks.
- **`harness-core`** — the `Agent` domain entity. All transitions are deterministic functions of current state + input; the integration test suite in `harness-core/tests/transitions.rs` pins this behavior.
- **`harness-runtime`** — the async layer: `SessionRuntime` (per-session event bus + command loop), `AgentRunner` (dispatches backend/tool/permission effects), `SessionManager` (multi-session lifecycle), cancellation tree, permissions module, scheduler and resource manager.
- **`harness-engine`** — the public, stable API (`Harness`, `SessionBuilder`, `SessionHandle`) that composes the runtime and the integration/tool registries. **This is the crate third-party Rust applications depend on.**
- **`harness-model` + `harness-generic-backend`** — the provider-neutral `ModelClient` trait and the `GenericModelBackend` adapter that adds retry/backoff/circuit-breaking on top of any client.
- **`harness-context`** — a backend decorator that injects the system prompt / workspace summary and truncates the transcript when it grows too large.
- **`harness-session-store`** — the `SessionStore` trait with `JsonlSessionStore` and `SqliteSessionStore` implementations.
- **`harness-extension-api`** — the semver-stable surface for third-party tools and backends (see [Extending the harness](#extending-the-harness)).

### Sessions, agents, and runs

A **session** is a workspace-bound conversation. Creating one resolves an integration name (e.g. `"anthropic"`) plus a provider-specific JSON config into a live backend, assembles a toolset, and starts a `SessionRuntime`. Each session owns one **root agent** which may spawn **child agents** (a tree, `parent_agent_id` on every envelope). A **run** is one unit of work triggered by a prompt; it streams through `Idle → PreparingContext → WaitingForBackend/Executing → … → Completed/Failed/Cancelled`, with intermediate states observed as events.

### The event model

Every observable occurrence is an `AgentEventEnvelope`:

```rust
pub struct AgentEventEnvelope {
    pub event_id: EventId,            // deduplication / replay
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub run_id: Option<RunId>,
    pub agent_sequence: u64,          // monotonic per agent
    pub session_sequence: Option<u64>, // monotonic per session (None until committed)
    pub timestamp: Timestamp,
    pub visibility: EventVisibility,  // User | Developer | Internal
    pub event: AgentEvent,
}
```

The two sequence numbers are the **ordering primitives** — never rely on timestamps alone when multiple agents run concurrently. There are 17 `AgentEvent` variants covering state transitions, run lifecycle, streaming text/reasoning deltas, tool calls, permission requests, usage updates, child-agent lifecycle, errors, and outcomes.

### Durability and resume

Each event variant is classified durable or ephemeral. Durable events are appended to the session store the moment they're committed; ephemeral ones are only ever broadcast live:

| Durable (persisted) | Ephemeral (live only) |
|---|---|
| `StateChanged`, `RunStarted` | `BackendRequestStarted`, `AssistantMessageStarted` |
| `AssistantMessageCompleted` (final text) | `AssistantTextDelta` (raw chunks) |
| `ToolCallStarted`, `ToolCallCompleted` (result summary) | `ToolCallRequested`, `ToolCallProgress` |
| `PermissionRequested`, `UsageUpdated` | `ReasoningDelta` |
| `ChildAgentSpawned`, `ChildAgentCompleted` | |
| `Failed`, `Completed` | |

Rationale: raw keystrokes are cheap to broadcast and expensive to store — only the *assembled* message/result is persisted. A client reconnecting mid-run therefore sees the final text, not the replayed chunks. This is the spec's explicit tradeoff.

**Resumable subscriptions** build on this: `Subscribe { since_seq: Some(n) }` replays every durable event with `session_sequence > n` (oldest first), then attaches to the live stream, deduplicating the overlap between backlog and broadcast. A TUI/IDE that remembers the highest `session_sequence` it saw can reconnect at any time with no gaps and no duplicates. Persistence across daemon restarts uses **snapshot + trailing events**: `SessionStore::load_session` returns the latest snapshot plus everything appended after it, and `Harness::restore_session` rebuilds a live `SessionHandle` from that.

### Provider resilience

Every model request runs through `GenericModelBackend::execute`, which provides:

1. **Retry** — transient failures (`RateLimited`, retryable `BackendError`, `Timeout`) are retried with exponential backoff (250 ms doubling) plus jitter, up to `max_attempts`.
2. **Shared deadline** — one `total_deadline` bounds all attempts and backoff delays combined; no unbounded retry loops.
3. **Circuit breaker** — `circuit_failure_threshold` consecutive transient failures open the circuit; while open, requests fail fast with a `CircuitOpen` error until `circuit_open_duration` elapses, then a single half-open probe is allowed.

The policy is a serializable struct embedded in every HTTP provider config as `recovery` (JSON keys: `max_attempts`, `total_deadline_secs`, `circuit_failure_threshold`, `circuit_open_duration_secs`). Defaults: `max_attempts: 2`, `total_deadline_secs: 15`, `circuit_failure_threshold: 3`, `circuit_open_duration_secs: 30`.

```console
--config-json '{"api_key":"sk-...","recovery":{"max_attempts":5,"total_deadline_secs":45}}'
```

The `claude-code`/`codex` subprocess backends bypass this layer (the CLI manages its own network retries); all HTTP backends go through it.

---

## Workspace layout

| Area | Crates | What it does |
|---|---|---|
| Core | `harness-protocol`, `harness-core`, `harness-runtime`, `harness-engine` | Wire types, the deterministic agent state machine, async orchestration, and the public `Harness`/`SessionBuilder` API |
| Context | `harness-context` | Injects a system prompt / workspace summary and truncates the transcript when it grows too large |
| Model backends | `crates/integrations/{anthropic,openai,openai-compatible,gemini}` | Direct HTTP API clients (Anthropic Messages API, OpenAI Chat Completions, any OpenAI-compatible endpoint, Gemini) |
| Subprocess backends | `crates/integrations/{claude-code,codex}` | Drive the `claude`/`codex` CLIs as subprocesses — the CLI manages its own tools and context, this just translates its output |
| Tools | `crates/tools/{filesystem,shell,git,web}` | `fs.read`/`fs.edit`/`workspace.search`, `shell.exec`, read-only `git.*`, `web.fetch` |
| Transports | `crates/transports/{ipc,websocket,stdio}` | Unix socket, WebSocket, and stdin/stdout framings of the same RPC contract (`harness_protocol::rpc`) |
| Apps | `apps/harnessd`, `apps/harnessctl`, `apps/harness` | The daemon, a reference CLI client, and a standalone interactive TUI |

The full development specification lives in [rust-agent-harness-development-spec.md](rust-agent-harness-development-spec.md).

---

## Quick start: run it and make a real request

Build everything first:

```console
cargo build --release
```

### Option A — no API key needed (if you have the Claude Code CLI installed and logged in)

```console
mkdir -p /tmp/demo-workspace

# Terminal 1: start the daemon
./target/release/harnessd --unix-socket /tmp/demo.sock --sessions-dir /tmp/demo-sessions

# Terminal 2: drive it
SID=$(./target/release/harnessctl --socket /tmp/demo.sock session create \
  --workspace /tmp/demo-workspace --integration claude-code \
  --config-json '{"sandbox_mode":"read-only","permission_mode":"bypassPermissions"}')

./target/release/harnessctl --socket /tmp/demo.sock session send "$SID" \
  "In one short sentence, what is a Rust trait?"

# poll for the answer...
./target/release/harnessctl --socket /tmp/demo.sock session snapshot "$SID"

# ...or stream it live instead (subscribe *before* sending the prompt to avoid missing events):
./target/release/harnessctl --socket /tmp/demo.sock session events "$SID"

./target/release/harnessctl --socket /tmp/demo.sock session close "$SID"
```

`session events` prints the ordered event stream as it happens, e.g.:

```
[0] StateChanged { from: Idle, to: PreparingContext }
[1] RunStarted { run_id: RunId(...) }
[2] StateChanged { from: PreparingContext, to: Streaming }
[3] AssistantTextDelta { message_id: MessageId(...), delta: "A trait defines a set of methods..." }
[4] StateChanged { from: Streaming, to: Idle }
[5] Completed { outcome: Success }
```

Pass `--json` for raw envelope JSON. If the connection drops mid-run (sleep/wake, window reload), re-subscribe from where you left off:

```console
# in the same stream, note the highest sequence printed (say 12), then reconnect:
./target/release/harnessctl --socket /tmp/demo.sock session events "$SID" --since-seq 12
```

Every durable event with `session_sequence > 12` is replayed first, then live events continue — no gaps, no duplicates. See [Durability and resume](#durability-and-resume) for what's replayable.

### Troubleshooting `claude-code`/`codex` subprocess spawn failures

These two backends spawn a real CLI as a child process, which means `harnessd` needs that CLI to actually be reachable from *its own* process environment — not just from whatever terminal you happen to be typing in. Two failure modes come up in practice:

**`Error: BackendError { message: "failed to spawn claude CLI: No such file or directory (os error 2)" ... }`**
`harnessd` inherits its `PATH` from whatever shell launched it. If `claude` isn't on `PATH` in *that* shell (e.g. `harnessd` was started before `nvm`/`asdf`/etc. initialized, or from a non-interactive shell), the plain-name lookup fails — even though `which claude` works fine when you check it in a normal interactive terminal afterward. Fix: skip `PATH` lookup entirely by pointing `binary_path` at the absolute path, found by running `which claude` in the *same terminal* you use to start `harnessd`:

```console
which claude   # run this in the terminal where harnessd is (or will be) started

--config-json '{"sandbox_mode":"read-only","permission_mode":"bypassPermissions","binary_path":"/absolute/path/from/which/claude"}'
```

If `which claude` prints nothing at all in that terminal, that's a real "not installed / not on PATH anywhere" problem to fix at the shell level first — no `binary_path` value will paper over that.

**`Please update your Node.js version or visit https://nodejs.org/ for additional instructions.`**
This comes from the `claude` CLI's own launcher script (it's a Node.js shim, `#!/usr/bin/env node`), not from the harness. It means the `node` binary that resolves on `PATH` in `harnessd`'s environment is too old — commonly caused by a `conda`/`pyenv`/etc. environment putting its own bundled `node` ahead of `nvm`'s in `PATH` (a `(base)` conda prompt is a strong hint). `binary_path` doesn't help here since the problem is one level down, inside the shebang's own `PATH` lookup for `node` — fix it by deactivating conda before starting `harnessd`, or reordering `PATH` so the right `node` wins.

Both of these are `claude-code`/`codex`-specific, since they're the only backends that spawn a subprocess at all — `anthropic`/`openai`/`gemini`/`openai-compatible` just make an HTTP call and never hit either issue.

### Option B — with a real model API key

Same as above with `--integration anthropic` (reads `ANTHROPIC_API_KEY`, or pass `--config-json '{"api_key":"sk-..."}'`), `openai` (`OPENAI_API_KEY`), or `gemini` (`GEMINI_API_KEY`). For `openai-compatible` (OpenRouter, a local Ollama/vLLM server, ...), the config JSON must include `base_url` and `model`, e.g.:

```console
--config-json '{"base_url":"http://localhost:11434/v1","model":"llama3"}'
```

All four HTTP integrations also accept `request_timeout_secs` and a `recovery` block (see [Provider resilience](#provider-resilience)).

### Enabling tools

By default a created session has no tools — useful for testing raw model plumbing, not for testing what the harness can actually *do*. Point `--workspace` at a real directory (ideally a git repo, to exercise `git.*`) and add:

```console
--tools fs.read,fs.edit,workspace.search,shell.exec,git.status,git.diff,git.log,git.show,web.fetch
```

or just `--all-tools` for the full set. Run `harnessctl session create --help` for the exact list — it's kept in sync with what the harness can actually build. Note: the `claude-code`/`codex` backends manage tools internally via their own CLI and never relay tool calls to the harness's registry, so `--tools` only does something observable with `anthropic`/`openai`/`gemini`/`openai-compatible`.

### `harnessctl chat` — an interactive TUI for manual testing

Composing `session create`/`send`/`events` by hand gets old fast. `harnessctl chat` does all three in one interactive terminal UI — a small ratatui client, same keybindings as `apps/harness`'s standalone TUI:

```console
./target/release/harnessctl --socket /tmp/demo.sock chat \
  --workspace /tmp/demo-workspace --integration claude-code \
  --config-json '{"sandbox_mode":"read-only","permission_mode":"bypassPermissions"}' \
  --all-tools
```

Type a prompt, press Enter, watch it stream in the activity pane. `y`/`n` approve or reject a pending tool permission, `Esc`/`Ctrl-C` cancels the active run, `q` closes the session and quits. It accepts the exact same `--integration`/`--config-json`/`--tools`/`--all-tools` flags as `session create`.

### Other ways to run it

- **`apps/harness`** — a standalone interactive TUI, no daemon involved: `ANTHROPIC_API_KEY=sk-... ./target/release/harness`.
- **`--tcp <addr>`** instead of `--unix-socket` starts the WebSocket transport (loopback-only, unauthenticated — see `crates/transports/websocket/src/lib.rs` before exposing it beyond `127.0.0.1`).
- **`--stdio`** starts the newline-delimited-JSON transport, for a parent process (an IDE) that spawns `harnessd` as a child and talks over its stdin/stdout. Multiple transport flags can be combined on one `harnessd` invocation.
- Sessions persist to `<cwd>/.harness/sessions` (JSONL) unless `--sessions-dir` points elsewhere.

---

## Integrating this into your own application

There are two integration shapes, depending on whether your application is Rust.

### 1. Embed the crates directly (Rust applications)

Depend on `harness-engine` (plus whichever integration/tool crates you need) and drive the same builder API `apps/harness`'s TUI uses:

```rust
let harness = Harness::builder()
    .register_integration(Arc::new(AnthropicFactory))
    .session_store(Arc::new(JsonlSessionStore::new(sessions_dir)))
    .build()
    .await?;

let session = harness
    .session()
    .integration("anthropic", AnthropicConfig::default())?
    .toolset(my_toolset, Arc::new(FsWorkspace::new(workspace_root)))
    .start()
    .await?;

let mut events = session.subscribe();
session.send("hello").await?;
```

`SessionHandle` exposes the full control surface: `send(prompt)`, `cancel()`, `resolve_permission(id, decision)`, `subscribe()` (live `broadcast::Receiver<AgentEventEnvelope>`), `snapshot()`, and `session_id()`. Persisted sessions can be resumed with `harness.restore_session(id).await?`. See `apps/harness/src/harness_setup.rs` for a complete working example, including how a toolset is assembled.

### 2. Talk to a running `harnessd` over the wire (any language)

This is the IDE-integration path — your application doesn't need to be Rust or link anything. Start `harnessd` (as a subprocess you spawn, or as a long-running service) and speak the RPC protocol defined in `harness_protocol::rpc` over whichever transport fits:

- **`--stdio`**: spawn `harnessd --stdio` as a child process, write one JSON-encoded request per line to its stdin, read one JSON-encoded response per line from its stdout. Closest to how a Language Server Protocol client works, and the simplest to adopt from any language with a subprocess API. Note: logging goes to stderr, never stdout, to protect this framing.
- **`--unix-socket <path>`**: length-prefixed JSON (4-byte little-endian length + payload) over a Unix domain socket. See `crates/transports/ipc/src/framing.rs` for the exact framing, and `apps/harnessctl/src/client.rs` for a complete reference client implementation to port from.
- **`--tcp <addr>`**: the same request/response shapes, one JSON value per WebSocket text message — no extra framing needed since WebSocket already delimits messages.

#### The wire protocol, step by step

The request/response contract (every variant of `RpcRequestBody`/`RpcResponseBody`) is defined in `crates/harness-protocol/src/rpc.rs` — start there. Every request carries a client-assigned correlation `id` echoed back on the matching response, so one connection can multiplex several sessions and in-flight requests.

**Step 0 — handshake (mandatory).** Every connection must start with `Hello`, or the daemon rejects everything else on that connection with an error:

```json
{ "id": 1, "session_id": null, "body": { "Hello": { "protocol_version": 1 } } }
```

A matching version gets `{ "Hello": { "protocol_version": 1, "capabilities": { "resumable_subscribe": true } } }`; a mismatch gets a clear `Error` instead of a confusing mid-session failure. The daemon advertises its capabilities here (currently `resumable_subscribe`), so clients can adapt without a version bump for every new optional feature.

Then the conversation flows:

1. `CreateSession { workspace_root, integration, integration_config, toolset }` → `SessionCreated { session_id }`.
2. `Subscribe { session_id, since_seq }` → `Ack`, then the daemon pushes `Event(AgentEventEnvelope)` frames on the same connection. Pass `since_seq: <n>` to first replay every durable event with `session_sequence > n` (see [Durability and resume](#durability-and-resume)); pass `null` for a fresh live-only stream. Pushed event frames carry `id: null` so they're distinguishable from request replies.
3. `Prompt(UserInput)` starts a run; watch pushed events for `AssistantTextDelta`/`ToolCallRequested`/`Completed`/etc.
4. `Snapshot` for a point-in-time status/usage read instead of watching the stream.
5. `ResolvePermission { id, decision }` when a tool call is gated behind a permission prompt (`permission_mode: "ask"`).
6. `Cancel` stops the active run; `CloseSession` tears the session down.

`apps/harnessctl` is both a usable CLI and the canonical worked example of this client-side protocol — read it end to end before writing a client in another language. Two `RpcRequestBody` variants (`Pause`/`Resume`) exist in the protocol but are not yet wired through the session engine API; the daemon returns an error for them today.

### Extending the harness

Third-party tools and model backends should depend on **`harness-extension-api`** — the only semver-stable extension surface (the internal `harness-tools`/`harness-runtime`/`harness-model` APIs may change between workspace versions). Implement `Plugin` to contribute tools (`Arc<dyn ToolExecutor>`) and/or integration factories, then hand a `Vec<Box<dyn Plugin>>` to your embedder at startup:

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn tools(&self) -> Vec<Arc<dyn ToolExecutor>> { Vec::new() }
    fn integrations(&self) -> Vec<Arc<dyn IntegrationFactory>> { Vec::new() }
}
```

Registration is **compile-time** by design in this version — plugins are linked into the host binary; dynamic loading (dylib/WASM) is a future extension.

---

## Development

The repository uses the stable Rust toolchain with Rust 1.78 as its minimum supported Rust version.

```console
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run --manifest-path xtask/Cargo.toml -- check-deps
```

Supply-chain checks use [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```console
cargo deny check
```

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.

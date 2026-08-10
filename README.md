# Rust Agent Harness

Rust Agent Harness is a reusable, embeddable runtime for building **tool-using AI agents**. It separates deterministic agent and session semantics from asynchronous execution, model providers, tools, persistence, transports, and user interfaces — so the same session behavior can run in a terminal, a daemon, an IDE, a TUI, or any other host application.

The repository is a Cargo workspace of small crates with explicit responsibility boundaries: a pure protocol/type layer, an async runtime, a public session-builder API, pluggable model backends, pluggable tools, durable persistence, and three wire transports that expose all of it to processes that aren't Rust at all.

This README covers what the engine does, how it works, every integration and tool it ships with, and the three ways to use it: **embedding the crates in a Rust application**, **talking to a `harnessd` daemon over a socket/stdio/WebSocket** (directly or via an SDK), or **running one of the ready-made apps** (`harness`, `harnessd` + `harnessctl`).

## Contents

- [Capabilities at a glance](#capabilities-at-a-glance)
- [How it works](#how-it-works) — layered architecture, sessions/agents/runs, the event model, durability and resume, protocol capabilities, provider resilience
- [Integrations](#integrations) — every model backend, its config shape, and where it's wired up
- [Tools](#tools) — everything an agent can call, including subagent delegation
- [Workspace layout](#workspace-layout)
- [Quick start](#quick-start-run-it-and-make-a-real-request)
- [Running the standalone TUI](#running-the-standalone-tui)
- [Integrating this into your own application](#integrating-this-into-your-own-application)
- [Extending the harness](#extending-the-harness)
- [Observability](#observability)
- [Development](#development)
- [Project status](#project-status)
- [License](#license)

---

## Capabilities at a glance

- **Deterministic agent state machine** — `harness-core` models an `Agent` as pure state with a single `apply()` transition function. Same inputs, same transitions, regardless of timing or transport; the async runtime only *executes* what the state machine decides.
- **Multi-session, multi-agent runtime** — `harness-runtime` runs any number of sessions concurrently (each with a root agent that can spawn child agents), with a shared scheduler, resource manager, and per-backend rate limiting.
- **Streaming event model** — every observable occurrence is an `AgentEventEnvelope` carrying routing metadata (`session_id`, `agent_id`, `parent_agent_id`, `run_id`) and two monotonic sequence numbers for exact ordering. Subscribers get a live push stream; reconnecting clients can **resume from a sequence number without gaps or duplicates**.
- **Durable session persistence** — every durable event is written to a `SessionStore` (JSONL or WAL-mode SQLite) as it happens, plus periodic state snapshots. Sessions survive daemon restarts and can be restored via `Harness::restore_session`. Raw streaming deltas stay ephemeral by design (see [Durability](#durability-and-resume)).
- **Seven pluggable model backends** — Anthropic Messages API, OpenAI Chat Completions, any OpenAI-compatible endpoint (OpenRouter, Ollama, vLLM, …), Gemini, and the `claude`/`codex`/`copilot` CLIs driven as subprocesses. All share one provider-neutral backend adapter. See [Integrations](#integrations) for exact config shapes and where each one is currently wired up.
- **Built-in resilience** — every HTTP model call goes through retry with exponential backoff + jitter, a shared deadline across attempts, and a circuit breaker. Settings are configurable per provider (see [Provider resilience](#provider-resilience)).
- **Pluggable tools** — filesystem read/edit/search, shell execution, read-only git, web fetch (with a built-in SSRF guard), and model-initiated subagent delegation (`agent.spawn`) ship out of the box, plus any tool an [MCP](#mcp-servers) server advertises over stdio; `harness-extension-api` is the stable surface for writing your own tools and backends (see [Extending the harness](#extending-the-harness)).
- **Permission gating** — tool calls can be configured `Allow` / `Ask` / (deny); pending requests surface as events and are resolved per-call (`y`/`n` in the TUIs, `ResolvePermission` on the wire).
- **Hierarchical cancellation** — a root `CancellationToken` fans out to every session, agent, backend request, and tool call; cancelling anywhere propagates and is idempotent.
- **Three wire transports, one RPC contract** — Unix domain socket (length-prefixed JSON), WebSocket, and stdio (newline-delimited JSON) all frame the same `RpcRequestBody`/`RpcResponseBody` types, with a mandatory `Hello` protocol-version handshake on every connection.
- **Works in-process or out-of-process** — the same engine is embedded directly by `apps/harness`'s TUI and exposed by `apps/harnessd` for external clients like `apps/harnessctl` or the Rust/TypeScript SDKs.

---

## How it works

### Layered architecture

The workspace is split into five layers, each depending only on the ones below it (enforced mechanically — see [Development](#development)):

```
┌────────────────────────────────────────────────────────────────┐
│ Apps          harness (TUI) · harnessd (daemon) · harnessctl   │
│               (CLI + chat TUI)                                  │
├────────────────────────────────────────────────────────────────┤
│ Transports    ipc · websocket · stdio                          │
│               (one RPC contract: harness_protocol::rpc)        │
├────────────────────────────────────────────────────────────────┤
│ Integrations  anthropic · openai · openai-compatible · gemini  │
│               · claude-code · codex · github-copilot           │
│ Tools         filesystem · shell · git · web · agent.spawn     │
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

- **`harness-protocol`** — pure serializable types only (no runtime, no I/O policy): requests/responses, events, commands, ids, usage, tools. This is the contract every transport and every wire client speaks. Versioned wire schemas are published as JSON Schema in [`schema/`](schema) (`protocol-v1.schema.json`, `protocol-v2.schema.json`).
- **`harness-core`** — the `Agent` domain entity. All transitions are deterministic functions of current state + input; the integration test suite in `harness-core/tests/transitions.rs` pins this behavior.
- **`harness-runtime`** — the async layer: `SessionRuntime` (per-session event bus + command loop), `AgentRunner` (dispatches backend/tool/permission effects), `AgentSupervisor` (enforces capability non-escalation on every spawned child), `SessionManager` (multi-session lifecycle), cancellation tree, permissions module, scheduler and resource manager.
- **`harness-engine`** — the public, stable API (`Harness`, `SessionBuilder`, `SessionHandle`) that composes the runtime and the integration/tool registries. **This is the crate third-party Rust applications depend on.**
- **`harness-model` + `harness-generic-backend`** — the provider-neutral `ModelClient` trait and the `GenericModelBackend` adapter that adds retry/backoff/circuit-breaking on top of any client.
- **`harness-context`** — a backend decorator that injects the system prompt / workspace summary and truncates the transcript when it grows too large.
- **`harness-session-store`** — the `SessionStore` trait with `JsonlSessionStore` and `SqliteSessionStore` implementations.
- **`harness-extension-api`** — the semver-stable surface for third-party tools and backends (see [Extending the harness](#extending-the-harness)).

### Sessions, agents, and runs

A **session** is a workspace-bound conversation. Creating one resolves an integration name (e.g. `"anthropic"`) plus a provider-specific JSON config into a live backend, assembles a toolset, and starts a `SessionRuntime`. Each session owns one **root agent** which may spawn **child agents** (a tree, `parent_agent_id` on every envelope) — either from Rust orchestration code or, as of the `agent.spawn` tool, from the model itself. A **run** is one unit of work triggered by a prompt; it streams through `Idle → PreparingContext → WaitingForBackend/Executing → … → Completed/Failed/Cancelled`, with intermediate states observed as events.

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

### Protocol capabilities

Every `Hello` handshake response carries a `ProtocolCapabilities` struct so a client never has to guess what the daemon it just connected to actually supports. Current defaults, straight from `harness_protocol::rpc::ProtocolCapabilities`:

| Capability | Value | Meaning |
|---|---|---|
| `resumable_subscribe` | `true` | `since_seq` resume works as described above |
| `lifecycle_commands` | `true` | session create/close/list over the wire |
| `typed_errors` | `true` | RPC errors carry a stable code/category, not just a string |
| `mutation_admission` | `true` | command IDs + optional expected-revision checks reject stale/duplicate mutations |
| `session_restore` | `true` | `restore_session` from a persisted snapshot |
| `event_gap_signals` | `true` | a client is told explicitly if it fell behind the broadcast buffer, rather than silently missing events |
| `durable_idempotency` | **`false`** | admission history (which command IDs were already accepted) is **not** persisted — it resets on daemon restart |
| `pause_resume` | **`false`** | no pause/resume run control yet, only cancel |

Treat the two `false` rows as real constraints, not roadmap trivia: a client that retries a mutation across a daemon restart using the same command ID cannot rely on the daemon recognizing it as a duplicate.

### Provider resilience

Every HTTP model request runs through `GenericModelBackend::execute`, which provides:

1. **Retry** — transient failures (`RateLimited`, retryable `BackendError`, `Timeout`) are retried with exponential backoff (250 ms doubling) plus jitter, up to `max_attempts`.
2. **Shared deadline** — one `total_deadline` bounds all attempts and backoff delays combined; no unbounded retry loops.
3. **Circuit breaker** — `circuit_failure_threshold` consecutive transient failures open the circuit; while open, requests fail fast with a `CircuitOpen` error until `circuit_open_duration` elapses, then a single half-open probe is allowed.

The policy is a serializable struct embedded in every HTTP provider config as `recovery` (JSON keys: `max_attempts`, `total_deadline_secs`, `circuit_failure_threshold`, `circuit_open_duration_secs`). Defaults: `max_attempts: 2`, `total_deadline_secs: 15`, `circuit_failure_threshold: 3`, `circuit_open_duration_secs: 30`.

```console
--config-json '{"api_key":"sk-...","recovery":{"max_attempts":5,"total_deadline_secs":45}}'
```

The `claude-code`/`codex`/`github-copilot` subprocess backends bypass this layer entirely (the CLI manages its own network retries); only the four HTTP backends go through it.

---

## Integrations

Seven model backends ship today, sharing one of two shapes: a **direct HTTP client** against the provider's own API, or a **subprocess driver** that shells out to a CLI the provider already publishes and translates its output into the same event stream.

| Integration | Crate | Kind | Credential source | Registered in `harnessd` | Registered in `apps/harness` (standalone TUI) |
|---|---|---|---|:---:|:---:|
| `anthropic` | `harness-integration-anthropic` | HTTP (Messages API) | `ANTHROPIC_API_KEY` or `api_key` in config | ✅ | ✅ |
| `openai` | `harness-integration-openai` | HTTP (Chat Completions) | `OPENAI_API_KEY` or `api_key` in config | ✅ | ✅ |
| `gemini` | `harness-integration-gemini` | HTTP | `GEMINI_API_KEY` or `api_key` in config | ✅ | not wired |
| `openai-compatible` | `harness-integration-openai-compatible` | HTTP (OpenAI-shaped) | optional `api_key` (some local servers need none) | ✅ | not wired |
| `claude-code` | `harness-integration-claude-code` | subprocess (`claude`) | CLI's own credential store (`claude` login) | ✅ | ✅ |
| `codex` | `harness-integration-codex` | subprocess (`codex`) | CLI's own credential store (`codex login`) | ✅ | ✅ |
| `github-copilot` | `harness-integration-github-copilot` | subprocess (`copilot`) | CLI's own credential store (`copilot login`) | ❌ **not yet** | ✅ |

`github-copilot` is a complete, tested integration (it's exercised by its own conformance suite like every other backend) that the standalone TUI already registers — it just isn't in `harnessd`'s `Harness::builder()` chain yet (`apps/harnessd/src/main.rs`). If you need it over the daemon/`harnessctl` path, that's a one-line `.register_integration(Arc::new(GitHubCopilotFactory))` addition, not a missing feature.

### Config shapes

Every HTTP integration's config is a flat JSON object passed via `--config-json` (CLI) or the second argument to `.integration(id, config)` (embedded). All four share `default_model`, `default_max_tokens`, `request_timeout_secs`, and a `recovery` block (see [Provider resilience](#provider-resilience)); the notable per-provider fields:

| Integration | Required fields | Notable optional fields |
|---|---|---|
| `anthropic` | none (reads `ANTHROPIC_API_KEY`) | `api_key`, `base_url` |
| `openai` | none (reads `OPENAI_API_KEY`) | `api_key`, `base_url`, `extra_headers` |
| `gemini` | none (reads `GEMINI_API_KEY`) | `api_key`, `base_url` |
| `openai-compatible` | `base_url`, `model` — no defaults, both required | `api_key` (omit for an unauthenticated local server), `extra_headers` |

The three subprocess integrations don't take API keys at all — they drive an already-authenticated CLI:

| Integration | Fields | Notes |
|---|---|---|
| `claude-code` | `binary_path` (default: resolve `claude` on `PATH`), `extra_args`, `permission_mode` (default `bypassPermissions` — the harness is the single permission layer), `timeout_secs` | See [subprocess troubleshooting](#troubleshooting-claude-codecodex-subprocess-spawn-failures) below |
| `codex` | `binary_path` (default `codex`), `extra_args`, `sandbox_mode` (`read-only` \| `workspace-write` \| `danger-full-access`, default `workspace-write`), `dangerously_bypass`, `working_dir` | `sandbox_mode`/`working_dir` only apply on a fresh session — `codex exec resume` doesn't accept `--sandbox`/`-C` and inherits the original session's policy |
| `github-copilot` | `binary_path` (default `copilot`), `model` (default `"auto"`), `working_dir` | |

Example — pointing `openai-compatible` at a local Ollama server:

```console
--config-json '{"base_url":"http://localhost:11434/v1","model":"llama3"}'
```

---

## Tools

An agent's toolset is assembled explicitly per session (`--tools ...` on the CLI, or `.toolset(...)` when embedding) — a session has **no tools by default**. Ten tools ship in the workspace:

| Tool ID | Crate | What it does | Mutates / executes? |
|---|---|---|:---:|
| `fs.read` | `harness-tool-filesystem` | Read a file from the bound workspace | no |
| `fs.edit` | `harness-tool-filesystem` | Create or modify a file | **yes** |
| `workspace.search` | `harness-tool-filesystem` | Search file contents/names in the workspace | no |
| `shell.exec` | `harness-tool-shell` | Run a shell command in the workspace | **yes** |
| `git.status` | `harness-tool-git` | `git status` | no |
| `git.diff` | `harness-tool-git` | `git diff` | no |
| `git.log` | `harness-tool-git` | `git log` | no |
| `git.show` | `harness-tool-git` | `git show` | no |
| `web.fetch` | `harness-tool-web` | Fetch a URL, with a built-in SSRF guard (blocks loopback, link-local, cloud-metadata, and RFC1918 targets by default — see `crates/tools/web/src/ssrf.rs`) | no |
| `agent.spawn` | `harness-runtime` (not a separate crate) | Model-facing subagent delegation (see below) | spawns a child agent |

Only `anthropic`/`openai`/`gemini`/`openai-compatible` sessions actually relay tool calls through this registry — `claude-code`/`codex`/`github-copilot` manage tools internally via their own CLI, so `--tools` has no observable effect on those three.

### `agent.spawn` — model-initiated subagent delegation

`agent.spawn` lets the model itself delegate a subtask to a child agent, through the exact same `AgentSupervisor`-enforced path that Rust-orchestrated spawning already uses — a model can never use this tool to grant a child more than the supervisor would allow via any other spawn path. Arguments:

| Field | Default | Notes |
|---|---|---|
| `task` (required) | — | becomes the child's first prompt |
| `role` | none | system-prompt framing, e.g. `"code reviewer"` |
| `tools` | a conservative read-only subset of the parent's own tools (`fs.read`, `workspace.search`, `git.*`, `web.fetch`) | never more than the parent actually has, delegatable and enabled — mutating tools (`fs.edit`, `shell.exec`) must be requested explicitly by name |
| `workspace` | `"read_only"` | `"inherit"` \| `"read_only"` \| `"snapshot"` \| `"new_worktree"` |
| `mode` | `"await"` | `"await"` blocks until the child finishes and folds its result into the tool call's result; `"concurrent"` returns immediately for fire-and-forget work |
| `budget` | inherits the parent's own budget | a model may only tighten limits, never loosen them |
| `model` | inherits the parent's model | can select a cheaper/faster model on the *same* provider (e.g. delegate to `claude-haiku-4-5`); cannot switch provider |

### MCP servers

Beyond the ten built-in tools, a session can connect any number of [MCP](https://modelcontextprotocol.io) servers over stdio — every tool the server advertises is discovered at session start and registered as `mcp.<server-name>.<tool-name>`, so it's indistinguishable from a built-in tool to the model and to `--tools`/permission gating. Implemented in `harness-tool-mcp` (`crates/tools/mcp`):

- **Handshake**: `initialize` → `notifications/initialized` → `tools/list` (with cursor pagination) → `tools/call`, hand-rolled JSON-RPC over stdio (no `rmcp` dependency — the workspace's own dependency-direction rule keeps that out of a `harness-tool-*` crate's transitive graph anyway).
- **Scope, deliberately**: stdio transport and `tools/*` only. No resources, prompts, sampling, roots, or the HTTP/streamable-HTTP transport — a server that only exposes those contributes nothing today.
- **Client only.** The harness can *consume* another MCP server's tools; it cannot *expose itself* as an MCP server for something like Claude Desktop or Cursor to connect into. That's unstarted, separate work.
- **Failure handling**: a request timeout, a malformed response, or the server process dying mid-call all become the same thing every other tool's error already is here — a logical `ToolResult { is_error: true }` the model can see and react to — not an aborted run. Spawn failures (bad `command`) surface earlier, at session start.

Reachable from all three entry points:

```console
# harnessctl (repeatable, name=command[,arg1,arg2,...]):
harnessctl session create --workspace ./repo --integration anthropic \
  --mcp-server 'filesystem=npx,-y,@modelcontextprotocol/server-filesystem,/tmp' \
  --tools fs.read

# apps/harness (standalone TUI), same flag shape, applies to every session
# the TUI starts for the rest of the run — including ones created later via
# the provider picker, which has no other way to carry config:
cargo run -p harness -- --mcp-server 'filesystem=npx,-y,@modelcontextprotocol/server-filesystem,/tmp'

# Embedding harness-engine directly: the real McpServerConfig builder.
```
```rust
let session = harness
    .session()
    .integration("anthropic", serde_json::json!({}))?
    .mcp_server(
        harness_engine::McpServerConfig::new("filesystem", "npx")
            .args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]),
    )
    .toolset(my_toolset, workspace)
    .start()
    .await?;
```

The `--mcp-server name=command[,args...]` CLI flag is a convenience for the common case; it can't express environment variables, a working directory, or a non-default request timeout. For those, use `RpcRequestBody::CreateSession.mcp_servers` directly over the wire (see [`McpServerSpec`](crates/harness-protocol/src/mcp.rs) — a plain serializable mirror of `McpServerConfig`, since `harness-protocol` can't depend on an I/O-bearing `harness-tool-*` crate) or `SessionBuilder::mcp_server` when embedding.

---

## Workspace layout

| Area | Crates | What it does |
|---|---|---|
| Core | `harness-protocol`, `harness-core`, `harness-runtime`, `harness-engine` | Wire types, the deterministic agent state machine, async orchestration, and the public `Harness`/`SessionBuilder` API |
| Context | `harness-context` | Injects a system prompt / workspace summary and truncates the transcript when it grows too large |
| Model backends | `crates/integrations/{anthropic,openai,openai-compatible,gemini}` | Direct HTTP API clients |
| Subprocess backends | `crates/integrations/{claude-code,codex,github-copilot}` | Drive the `claude`/`codex`/`copilot` CLIs as subprocesses — the CLI manages its own tools and context, this just translates its output |
| Tools | `crates/tools/{filesystem,shell,git,web,mcp}` | `fs.read`/`fs.edit`/`workspace.search`, `shell.exec`, read-only `git.*`, `web.fetch`, an MCP client (`agent.spawn` lives in `harness-runtime` itself, see [Tools](#tools)) |
| Transports | `crates/transports/{ipc,websocket,stdio}` | Unix socket, WebSocket, and stdin/stdout framings of the same RPC contract (`harness_protocol::rpc`) |
| Apps | `apps/harnessd`, `apps/harnessctl`, `apps/harness` | The daemon, a reference CLI client, and a standalone interactive TUI |
| SDKs | `sdk/rust`, `sdk/typescript` | Application-agnostic client facades — see [SDKs](#sdks) |

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

### Troubleshooting `claude-code`/`codex`/`github-copilot` subprocess spawn failures

These three backends spawn a real CLI as a child process, which means `harnessd` needs that CLI to actually be reachable from *its own* process environment — not just from whatever terminal you happen to be typing in. Two failure modes come up in practice:

**`Error: BackendError { message: "failed to spawn claude CLI: No such file or directory (os error 2)" ... }`**
`harnessd` inherits its `PATH` from whatever shell launched it. If `claude` isn't on `PATH` in *that* shell (e.g. `harnessd` was started before `nvm`/`asdf`/etc. initialized, or from a non-interactive shell), the plain-name lookup fails — even though `which claude` works fine when you check it in a normal interactive terminal afterward. Fix: skip `PATH` lookup entirely by pointing `binary_path` at the absolute path, found by running `which claude` in the *same terminal* you use to start `harnessd`:

```console
which claude   # run this in the terminal where harnessd is (or will be) started

--config-json '{"sandbox_mode":"read-only","permission_mode":"bypassPermissions","binary_path":"/absolute/path/from/which/claude"}'
```

If `which claude` prints nothing at all in that terminal, that's a real "not installed / not on PATH anywhere" problem to fix at the shell level first — no `binary_path` value will paper over that. The same applies verbatim to `codex`/`which codex` and `github-copilot`/`which copilot`.

**`Please update your Node.js version or visit https://nodejs.org/ for additional instructions.`**
This comes from the `claude` CLI's own launcher script (it's a Node.js shim, `#!/usr/bin/env node`), not from the harness. It means the `node` binary that resolves on `PATH` in `harnessd`'s environment is too old — commonly caused by a `conda`/`pyenv`/etc. environment putting its own bundled `node` ahead of `nvm`'s in `PATH` (a `(base)` conda prompt is a strong hint). `binary_path` doesn't help here since the problem is one level down, inside the shebang's own `PATH` lookup for `node` — fix it by deactivating conda before starting `harnessd`, or reordering `PATH` so the right `node` wins.

All three subprocess backends are the only ones that spawn a subprocess at all — `anthropic`/`openai`/`gemini`/`openai-compatible` just make an HTTP call and never hit either issue.

### Option B — with a real model API key

Same as above with `--integration anthropic` (reads `ANTHROPIC_API_KEY`, or pass `--config-json '{"api_key":"sk-..."}'`), `openai` (`OPENAI_API_KEY`), or `gemini` (`GEMINI_API_KEY`). For `openai-compatible` (OpenRouter, a local Ollama/vLLM server, ...), the config JSON must include `base_url` and `model`, e.g.:

```console
--config-json '{"base_url":"http://localhost:11434/v1","model":"llama3"}'
```

See [Integrations](#integrations) for the complete config reference across all seven backends.

### Enabling tools

By default a created session has no tools — useful for testing raw model plumbing, not for testing what the harness can actually *do*. Point `--workspace` at a real directory (ideally a git repo, to exercise `git.*`) and add:

```console
--tools fs.read,fs.edit,workspace.search,shell.exec,git.status,git.diff,git.log,git.show,web.fetch,agent.spawn
```

or just `--all-tools` for the full set. Run `harnessctl session create --help` for the exact list — it's kept in sync with what the harness can actually build. Note: the `claude-code`/`codex`/`github-copilot` backends manage tools internally via their own CLI and never relay tool calls to the harness's registry, so `--tools` only does something observable with `anthropic`/`openai`/`gemini`/`openai-compatible`.

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

- **`apps/harness`** — a standalone interactive TUI, no daemon involved: `ANTHROPIC_API_KEY=sk-... ./target/release/harness`. See [Running the standalone TUI](#running-the-standalone-tui).
- **`--tcp <addr>`** instead of `--unix-socket` starts the WebSocket transport (loopback-only, unauthenticated — see `crates/transports/websocket/src/lib.rs` before exposing it beyond `127.0.0.1`).
- **`--stdio`** starts the newline-delimited-JSON transport, for a parent process (an IDE) that spawns `harnessd` as a child and talks over its stdin/stdout. Multiple transport flags can be combined on one `harnessd` invocation.
- Sessions persist to `<cwd>/.harness/sessions` (JSONL) unless `--sessions-dir` points elsewhere.

---

## Running the standalone TUI

`apps/harness` runs the engine in-process — no daemon required — and currently registers all seven integrations, including `github-copilot`. Full walkthrough (provider setup, keyboard shortcuts, session storage, troubleshooting) lives in [`apps/harness/README.md`](apps/harness/README.md); the short version:

```console
cargo run -p harness                              # provider picker, defaults to Anthropic
cargo run -p harness -- --integration codex        # start with a specific provider
```

---

## Integrating this into your own application

There are three integration shapes, depending on whether your application is Rust and whether it wants the engine in-process.

### 1. Embed the crates directly (Rust applications)

Depend on `harness-engine` (plus whichever integration/tool crates you need) and drive the same builder API `apps/harness`'s TUI uses. This is the full pattern, adapted from the working example at [`sdk/rust/examples/basic_chat.rs`](sdk/rust/examples/basic_chat.rs):

```rust
use std::sync::Arc;

use harness_engine::Harness;
use harness_protocol::events::AgentEvent;
use harness_protocol::events::AgentOutcome;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let harness = Harness::builder()
        .register_integration(Arc::new(harness_integration_anthropic::AnthropicFactory))
        .build()
        .await?;

    let session = harness
        .session()
        .integration("anthropic", serde_json::json!({}))?
        .start()
        .await?;

    let mut events = session.subscribe(); // tokio::sync::broadcast::Receiver<AgentEventEnvelope>
    session.send("In one short sentence, what is a Rust trait?").await?;

    while let Ok(envelope) = events.recv().await {
        match envelope.event {
            AgentEvent::AssistantTextDelta { delta, .. } => print!("{delta}"),
            AgentEvent::Completed { outcome } => {
                println!();
                if !matches!(outcome, AgentOutcome::Success) {
                    eprintln!("run ended with outcome: {outcome:?}");
                }
                break;
            }
            _ => {}
        }
    }

    session.close().await?;
    Ok(())
}
```

Add `.session_store(Arc::new(JsonlSessionStore::new(sessions_dir)))` to the builder for durable persistence across restarts, `.tools(...)`/`.toolset(...)` for a tool registry (see [Tools](#tools)), and `.context_provider(...)` to wire in transcript compaction (`harness-context`). Prefer the curated `rusty-harness-sdk` facade over depending on `harness-engine` directly if you don't need the full internal surface — see [SDKs](#sdks) below.

### 2. Talk to `harnessd` over the wire (any language, or a sandboxed/out-of-process Rust host)

Start `harnessd` with `--unix-socket`, `--tcp`, and/or `--stdio` (see [Quick start](#quick-start-run-it-and-make-a-real-request)) and speak the shared RPC contract directly, or through:

- **The TypeScript SDK** (`@rusty/harness-sdk`, in [`sdk/typescript`](sdk/typescript)) — the reference client for the wire protocol today: handshake, session lifecycle, prompt/steer/follow-up/cancel/permission mutations, typed structured errors, and resumable event subscription with explicit gap callbacks.
- **The raw protocol** — `harnessctl`'s own source (`apps/harnessctl/src`) is a second working reference client. The wire shapes are published as JSON Schema in [`schema/protocol-v2.schema.json`](schema/protocol-v2.schema.json); the Rust types in `harness-protocol` are the authoritative source of truth the schema mirrors.

This is the integration shape an IDE would normally use: spawn `harnessd --stdio` as a child process (or connect to an already-running `--unix-socket`/`--tcp` instance) and never link Rust into the host process at all.

### SDKs

| SDK | Package | Status | Best for |
|---|---|---|---|
| Rust | [`sdk/rust`](sdk/rust) (`rusty-harness-sdk`) | alpha, compiles against the current workspace | Embedding in-process without depending on internal crates directly |
| TypeScript | [`sdk/typescript`](sdk/typescript) (`@rusty/harness-sdk`) | alpha, source-only, not yet published | Talking to `harnessd` over stdio today; WebSocket/IPC transports are planned, not yet implemented |
| Java | not started | — | Tracked as a future phase, no code yet |

Both existing SDKs document their own real, current gaps inline rather than hiding them — worth reading before depending on either for anything beyond prototyping. Highlights: the Rust SDK doesn't yet expose `steer`/`follow_up`/`close_session` or attachments (engine-level gaps, not SDK-specific); the TypeScript SDK's `HarnessClient.capabilities.durable_idempotency` is `false` for the reason in [Protocol capabilities](#protocol-capabilities). Full detail: [`sdk/README.md`](sdk/README.md), [`sdk/rust/README.md`](sdk/rust/README.md), [`sdk/typescript/README.md`](sdk/typescript/README.md).

---

## Extending the harness

`harness-extension-api` is the one crate third-party tool/backend authors should depend on — `harness-tools`, `harness-runtime`, `harness-model`, and `harness-generic-backend` are implementation details that can change shape between workspace versions without warning; this crate follows semver strictly instead.

Implement the `Plugin` trait to contribute tools and/or integrations:

```rust
use harness_extension_api::{Plugin, ToolExecutor, IntegrationFactory};
use std::sync::Arc;

struct MyPlugin;

impl Plugin for MyPlugin {
    fn name(&self) -> &'static str { "my-plugin" }

    fn tools(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![/* your ToolExecutor impls */]
    }

    fn integrations(&self) -> Vec<Arc<dyn IntegrationFactory>> {
        vec![/* your IntegrationFactory impls */]
    }
}
```

An embedding host collects `Vec<Box<dyn Plugin>>` at startup and folds `.tools()`/`.integrations()` into its registries.

**Current scope, plainly stated:** this is **compile-time registration only** — a plugin is a Rust crate the host links in and recompiles with, exactly like the built-in integrations and tools. There is no dynamic loading (dylib/WASM) yet; ABI stability across a dylib boundary, sandboxing, and versioning are real, unstarted problems, not a small addition. If your use case is "install a third-party plugin without rebuilding the host," that's not supported today.

---

## Observability

`harnessd` installs a process-wide Prometheus recorder at startup (`metrics_exporter_prometheus`) and every layer of the workspace emits counters/histograms/gauges through the lightweight `metrics` facade — no crate below the daemon binary itself needs to know Prometheus specifically exists. Metrics text isn't served over its own HTTP endpoint; it's returned on demand by the `GetDiagnostics` RPC alongside scheduler/session-manager state, so a client that already has a connection to `harnessd` can pull metrics without opening a second port.

---

## Development

```console
cargo build --workspace --all-targets      # build everything
cargo test --workspace --all-targets       # run the full test suite
cargo fmt --all -- --check                 # formatting (stable toolchain — see rust-toolchain.toml)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps            # RUSTDOCFLAGS="-D warnings" in CI
cargo run -p xtask -- check-deps           # enforces the layered dependency direction above
```

CI (`.github/workflows/ci.yml`) runs all of the above, plus a 3-OS × 3-toolchain (`stable`, MSRV `1.78`, `beta`) test matrix, a separate TypeScript SDK build+test job, and `cargo-deny` for license/advisory/supply-chain checks (`deny.toml`).

---

## Project status

Versions across the workspace are `0.1.x` — nothing here is published to crates.io or npm yet, and no crate makes a semver promise except `harness-extension-api`. Known, current limitations worth knowing before depending on this:

- MCP support is client-only (the harness can consume another MCP server's tools; it cannot expose itself as one — see [MCP servers](#mcp-servers)), and stdio/`tools`-only within that.
- The WebSocket transport is unauthenticated and loopback-only by deliberate scope decision (see `crates/transports/websocket/src/lib.rs`) — a remote/multi-tenant deployment needs an auth/TLS layer built on top, not just a different bind address.
- `github-copilot` isn't yet registered in `harnessd` (see [Integrations](#integrations)).
- Extension loading is compile-time only (see [Extending the harness](#extending-the-harness)) — no third-party plugin ecosystem without recompiling the host.
- `durable_idempotency` and `pause_resume` are `false` in `ProtocolCapabilities` (see [Protocol capabilities](#protocol-capabilities)).
- No independent external security review has been done on this codebase.

None of these are silently papered over — each is called out at the point in this README (or in the source doc comment it links to) where it actually matters, rather than only here.

---

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

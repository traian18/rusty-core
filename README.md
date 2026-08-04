# Rust Agent Harness

Rust Agent Harness is a reusable, embeddable Rust runtime for building tool-using AI agents. It separates deterministic agent and session semantics from asynchronous execution, model providers, tools, persistence, transports, and user interfaces so the same session behavior can run in a terminal, daemon, IDE, or host application.

This repository is organized as a Cargo workspace containing small crates with explicit responsibility boundaries: a pure protocol/type layer, an async runtime, a public session-builder API, pluggable model backends, pluggable tools, and a transport layer that exposes all of it to processes that aren't Rust at all.

## Project documentation

- [Development specification](rust-agent-harness-development-spec.md)

## What's in the workspace

| Area | Crates | What it does |
|---|---|---|
| Core | `harness-protocol`, `harness-core`, `harness-runtime`, `harness-engine` | Wire types, the deterministic agent state machine, async orchestration, and the public `Harness`/`SessionBuilder` API |
| Context | `harness-context` | Injects a system prompt / workspace summary and truncates the transcript when it grows too large |
| Model backends | `crates/integrations/{anthropic,openai,openai-compatible,gemini}` | Direct HTTP API clients (Anthropic Messages API, OpenAI Chat Completions, any OpenAI-compatible endpoint, Gemini) |
| Subprocess backends | `crates/integrations/{claude-code,codex}` | Drive the `claude`/`codex` CLIs as subprocesses — the CLI manages its own tools and context, this just translates its output |
| Tools | `crates/tools/{filesystem,shell,git,web}` | `fs.read`/`fs.edit`/`workspace.search`, `shell.exec`, read-only `git.*`, `web.fetch` |
| Transports | `crates/transports/{ipc,websocket,stdio}` | Unix socket, WebSocket, and stdin/stdout framings of the same RPC contract (`harness_protocol::rpc`) |
| Apps | `apps/harnessd`, `apps/harnessctl`, `apps/harness` | The daemon, a reference CLI client, and a standalone interactive TUI |

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

See `apps/harness/src/harness_setup.rs` for a complete working example, including how a toolset is assembled.

### 2. Talk to a running `harnessd` over the wire (any language)

This is the IDE-integration path — your application doesn't need to be Rust or link anything. Start `harnessd` (as a subprocess you spawn, or as a long-running service) and speak the RPC protocol defined in `harness_protocol::rpc` over whichever transport fits:

- **`--stdio`**: spawn `harnessd --stdio` as a child process, write one JSON-encoded request per line to its stdin, read one JSON-encoded response per line from its stdout. Closest to how a Language Server Protocol client works, and the simplest to adopt from any language with a subprocess API.
- **`--unix-socket <path>`**: length-prefixed JSON (4-byte little-endian length + payload) over a Unix domain socket. See `crates/transports/ipc/src/framing.rs` for the exact framing, and `apps/harnessctl/src/client.rs` for a complete reference client implementation to port from.
- **`--tcp <addr>`**: the same request/response shapes, one JSON value per WebSocket text message — no extra framing needed since WebSocket already delimits messages.

The request/response contract (every variant of `RpcRequestBody`/`RpcResponseBody`) is defined in `crates/harness-protocol/src/rpc.rs` — start there. In short:

1. Send `CreateSession { workspace_root, integration, integration_config, toolset }` → get back `SessionCreated { session_id }`.
2. Send `Subscribe` (with that `session_id`) to start receiving `Event(AgentEventEnvelope)` frames pushed on the same connection.
3. Send `Prompt(UserInput)` to start a run; watch the pushed events for `AssistantTextDelta`/`Completed`/etc.
4. Send `Snapshot` for a point-in-time status/usage read instead of watching the stream.
5. Send `ResolvePermission` when a tool call is gated behind a permission prompt.
6. Send `CloseSession` when done.

`apps/harnessctl` is both a usable CLI and the canonical worked example of this client-side protocol — read it end to end before writing a client in another language.

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

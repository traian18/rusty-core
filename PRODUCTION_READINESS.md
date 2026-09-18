# Production readiness — remaining work

Tracking doc for making `rusty-core` a version the `rusty` IDE can ship on.
Supersedes the gap analysis in `../RUSTY_CORE_INTEGRATION_PLAN.md` §3 for the
rusty-core side only.

Decisions taken: **Windows is a shipping target**, and **provider auth is
upstreamed into `harness-engine`** rather than left in rusty.

Baseline when this work started: `cargo test --workspace --all-targets` green
(718 tests / 64 suites), clean tree, no `todo!()`/`unimplemented!()`.

---

## Done

### Silent fake-workspace fallback removed — *correctness*

`SessionBuilder::start` bound `FakeWorkspace` (in-memory) when no workspace
was given. A session with real tools but a forgotten `.workspace(...)` read
files as "not found" and wrote into a buffer that was discarded — with
successful tool results either way. For an IDE whose agent edits the user's
real files, that is a silent-data-loss failure mode.

Replaced with `harness_workspace::UnboundWorkspace`, which fails every
operation with the new `WorkspaceError::Unbound`. Sessions with no
workspace-touching tools are unaffected (nothing calls it); sessions that do
get an actionable error at the first tool call. All 718 existing tests still
pass, so nothing depended on the old behavior.

`Harness::restore_session` still binds a fake deliberately — that one is
documented and directs callers to `restore_session_with_toolset`.

### Test doubles no longer ship in release builds — *build weight*

- `harness-generic-backend/src/lib.rs` declared a `testing` feature but left
  `pub mod testing;` **unconditional**, so `FakeModelClient` and the contract
  suite compiled into every downstream build. Now gated.
- `harness-engine` and `sdk/rust` enabled `harness-runtime/testing` in
  `[dependencies]`. Cargo unifies features across the graph, so this turned
  `FakeBackend`/`FakeToolRegistry` on for **every consumer's release build**.
  Neither crate's `src/` ever used them. Moved to `[dev-dependencies]`
  (engine) and dropped entirely (sdk).

### Pause/resume wired through — *feature parity*

The deterministic core already implemented it (`AgentStatus::Paused`,
`transitions::{pause,resume}`, `SessionCommand::Pause`/`Resume`) but nothing
exposed it. Added, end to end:

- `SessionHandle::pause()` / `resume()`
- `MutationCommand::Pause` / `Resume` + `harnessd` handler dispatch
- `harnessctl session pause|resume`
- TypeScript SDK `client.pause/resume`, `session.pause/resume`, and the
  `MutationCommand` union member
- `schema/protocol-v2.schema.json` variants
- `ProtocolCapabilities::pause_resume` flipped to `true`
- `crates/harness-engine/tests/pause_resume_e2e.rs` (3 tests) + a wire
  round-trip test in `harness-protocol`

Semantics worth knowing: pause is recoverable (resume continues the *same*
run, re-issuing `ExecuteBackend` for the same `run_id`), and it does **not**
abort an in-flight backend request — `AgentRunner::run` receives one command,
then awaits `dispatch_effects` before receiving the next, so a pause issued
mid-stream applies once that request settles. Documented on the method;
deliberately not asserted in tests, since pinning it would pin timing.

### CI actually runs — *hygiene*

`.github/workflows/ci.yml` triggered on `workflow_dispatch` only, so nothing
ran on push or PR. Consequence: `cargo fmt --check` had been **failing on
`main`** — four files under `crates/harness-runtime/src/session_manager/`
left unformatted by the recent refactoring commits, unnoticed because the
gate never ran. Added `push` (main/master) and `pull_request` triggers, and
formatted the four files.

### `github-copilot` registered in `harnessd`

The README described this as "a one-line addition, not a missing feature".
Done — all seven integrations are now reachable over the daemon path, which
rusty needs since its sidecar has a working Copilot service.

### Build weight: SQLite no longer compiled into anything that doesn't use it

`harness-session-store` pulled `rusqlite` with `bundled` unconditionally —
compiling SQLite from C — and every intermediate crate took it with default
features, so it reached every binary. Nothing outside `sdk/rust` (which
re-exports `SqliteSessionStore`) actually referenced it; `harnessd` and
`apps/harness` both use `JsonlSessionStore`.

Now: `sqlite` is an on-by-default feature of `harness-session-store`, and
`harness-runtime` / `harness-engine` / `harnessd` / `apps/harness` take the
crate with `default-features = false`. Measured with `cargo tree`:

| Crate | `rusqlite`/`libsqlite3-sys` in graph |
|---|---|
| `harnessd` | 0 (was 3) |
| `apps/harness` | 0 (was 3) |
| `harness-engine` *(what rusty embeds)* | 0 (was 3) |
| `rusty-harness-sdk` | 3 — correct, it re-exports the type |

A `default-features = false` consumer was verified to build against
`JsonlSessionStore` with zero sqlite crates in its tree.

Also gated `harness-session-store`'s `testing` module (`MemoryStore`,
`FaultInjectingStore`) — the third instance of the same
unconditional-test-doubles bug — and deleted `src/store.rs.patch`, a stray
empty file that was checked into git.

**Investigated and deliberately not done:** feature-gating the tool crates
(`harness-tool-git`, `-web`, etc.) out of `harness-engine`. It would remove
no native compilation: `git2`/`libgit2-sys` arrives via `harness-workspace`,
which `harness-engine` requires unconditionally for the `Workspace` trait and
worktree support, and `reqwest` is a direct `harness-engine` dependency.
Gating them would be churn with a measured payoff of zero.

### Structured output — *new capability*

`ExecutionParams::response_format` (`Text` / `JsonObject` /
`JsonSchema { name, schema, strict }`), provider-neutral, plus
`BackendCapabilities::structured_output` so an unsatisfiable request is
rejected **before** a billed call rather than returning prose the caller
then fails to parse.

| Integration | Mechanism |
|---|---|
| `openai`, `openai-compatible` | native `response_format` (one impl — openai-compatible reuses `OpenAiClient`) |
| `gemini` | `responseMimeType` + `responseSchema` |
| `anthropic` | emulated: a forced single-purpose tool whose input schema *is* the requested schema, its input surfaced as assistant text |
| the three subprocess backends | unsupported; capability is `false` |

Two things worth recording:

- **A bug caught while wiring it.** The four HTTP integrations set
  `structured_output: true` on their *factory descriptor*, but
  `GenericModelBackend` derives its capabilities from the client's
  `ModelCapabilities` — where the field defaulted to `false`. Every
  structured-output request would have been rejected by the very backend
  advertising support for it. The capability now lives on `ModelCapabilities`
  where it belongs, and `structured_output_capability_propagates_from_the_client`
  pins it.
- **Gemini schema sanitization.** Gemini's `responseSchema` accepts a
  restricted subset; `$schema`/`$id`/`additionalProperties`/`$defs`/
  `definitions` are stripped recursively, since a `schemars`-generated schema
  carries all of them and would 400 the whole request.

14 new tests, including one asserting the Anthropic emulation reassembles as
text and emits no tool-call events, and one asserting a *real* tool call in
the same stream is unaffected.

### Doc drift corrected

- README claimed MCP was "client-only … cannot expose itself as one" while
  documenting MCP server mode at length two sections earlier. Fixed.
- README `pause_resume`/`github-copilot` status rows updated.
- `merry-weaving-riddle.md` still listed Phase 3 (MCP server mode) as "Not
  started" though `233cb88` shipped it.

---

## Remaining

Ordered by dependency, cheapest first within a tier.

### 1. `Workspace` trait is too narrow for rusty's VFS — **blocks integration**

`read / write / search / list_files`, and `read` returns `String`. rusty's
VFS also deletes, renames, and creates directories, and its tree contains
binary files.

Options: extend the trait (breaks every implementor — there are five), or add
the extras as `ToolExecutor`s registered per session. **Start with custom
tools**; only widen the trait if rusty's `VfsWorkspace` proves it needs to.
Binary reads need a decision either way — likely a `read_bytes` with a
default impl that errors, so existing implementors don't break.

### 2. Box `SpawnAgentSpec` in `AgentCommand::SpawnChild` — small, deferred

`SpawnAgentSpec` makes `AgentCommand` ~464 bytes, so every command — even
`Cancel`/`Pause` — costs that much in a channel buffer. `clippy::large_enum_variant`
flags it; `AgentCommand` and `SessionCommand` both carry an `#[allow]` for it.

Boxing is wire-compatible (`Box<T>` serializes identically to `T`) but
ripples into `AgentEffect::SpawnAgent` and `harness-core`'s transition
function, so it wants to be its own change rather than a rider on an
unrelated one.

### 3. Windows support — **decided in scope, but not locally verifiable**

`transports/ipc` and `harnessctl`'s client use `tokio::net::UnixListener`/
`UnixStream` unconditionally, so the workspace does not build on Windows at
all today. CI's matrix is `ubuntu-latest`/`macos-latest` only.

The embedded path rusty actually uses is unaffected — it links
`harness-engine` directly and never opens a socket. So the work is: `cfg`
out the Unix-only surface (`transports/ipc`, `harnessctl`'s socket client,
`harnessd --unix-socket`), then add `windows-latest` to CI. Scope it as "the
workspace builds and tests pass on Windows", not "the IPC transport works on
Windows" — named pipes are a separate, larger project and nothing needs them
yet.

**Verification caveat, decide before starting:** this cannot be checked from
this machine. `cargo check --target x86_64-pc-windows-msvc` fails in
`libz-sys`/`libsqlite3-sys` build scripts — cross-compiling their C from
macOS needs a Windows toolchain — and `libz-sys` is unavoidable because
`git2` comes in via `harness-workspace`. So the `cfg` work would be written
blind and only proven when CI runs on `windows-latest`. That's acceptable
for mechanical `cfg` gating, but it should be a conscious choice rather than
a claim that it works.

### 4. Provider auth + quota upstream — **largest item**

`Harness::begin_auth` (`harness-engine/src/harness.rs:81`) returns
`WaitingForExternalCommand { program: "copilot", args: ["login"] }` — it
tells the caller to go run the CLI, it never drives the flow.
`list_credential_profiles` hardcodes env-var checks for two providers. Quota
does not exist as a concept.

The working implementation to port lives in rusty's sidecar (~2100 LOC):
`copilotService.ts`, `codexService.ts`, `claudeCodeService.ts`,
`providerQuota.ts`. Target: `harness-engine::providers`, so `harnessd` and
the TUI gain it too.

### 5. `durable_idempotency`

`AdmissionCache` is in-memory in `apps/harnessd/src/handler.rs`, so a
mutation retried across a daemon restart is not recognized as a duplicate.
**Low priority** for an embedded single-user desktop app — rusty links the
engine directly and never restarts a daemon mid-session. Leave the capability
honestly `false`.

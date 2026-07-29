# Phase 2 — Single-Session Runtime

**Goal (spec Section 71):** `session.send(prompt)` / `session.subscribe()` works end-to-end for one session, using a fake backend and fake tool registry — no real provider yet.
**Depends on:** Phase 1 complete.
**Crates touched:** `harness-runtime` (new domain code), `harness-core` (minor extensions if gaps found), `harness-protocol` (minor extensions if gaps found).

---

## Tasks

### Task 2.1 — `AgentTask` mailbox primitive
- **Files:** `crates/harness-runtime/src/agent_runner.rs`
- **Description:** Implement `AgentTask` (spec Section 32): `id: AgentId`, `commands: mpsc::Receiver<AgentCommand>`, `events: broadcast::Sender<AgentEventEnvelope>`, `cancel: CancellationToken`. Add `tokio` (`sync`, `rt`, `macros` features) as a real dependency of `harness-runtime` for the first time in the project — this is the intended crate boundary where async enters (spec Section 66: core stays sync/deterministic, runtime executes).
- **Acceptance criteria:** `AgentTask` constructible; a unit test sends a command through the mpsc channel and receives it on the other end inside a `#[tokio::test]`.
- **Effort:** S
- **Depends on:** Phase 1 complete

### Task 2.2 — `AgentRunner` async loop
- **Files:** `crates/harness-runtime/src/agent_runner.rs`
- **Description:** Implement `AgentRunner` (spec Section 10): owns an `Agent` (from `harness-core`) and an `AgentTask`; runs an async loop that:
  1. receives an `AgentCommand` from the mailbox,
  2. calls `agent.apply(command)` (pure, synchronous),
  3. interprets each returned `AgentEffect` by dispatching to the appropriate runtime dependency (backend call, tool call, spawn, permission request, persist, emit, finish),
  4. forwards `AgentEffect::Emit(event)` onto the `broadcast::Sender<AgentEventEnvelope>` (wrapping with sequence/session/agent IDs).
  This loop must treat `ExecuteBackend`/`ExecuteTool`/`SpawnAgent` as **fire against injected trait objects**, not concrete types — in this phase those trait objects are the fakes from Tasks 2.4/2.5.
- **Acceptance criteria:** given a `FakeBackend` (Task 2.4) scripted to emit a text delta then complete, running the loop from `StartRun` to `Completed` produces the expected ordered event stream on the broadcast channel.
- **Effort:** L
- **Depends on:** Task 2.1

### Task 2.3 — Cancellation wiring
- **Files:** `crates/harness-runtime/src/cancellation.rs`, `crates/harness-runtime/src/agent_runner.rs`
- **Description:** Root `CancellationToken` created per session (Task 2.7); `AgentRunner` derives `.child_token()` for its own run and passes it into every backend/tool call per spec Section 35. Implement `SessionCommand::CancelRun`/`CancelAgent` → `AgentCommand::Cancel` translation (full wiring finishes once `SessionRuntime` exists in Task 2.7).
- **Acceptance criteria:** a unit test starts a `FakeBackend` call that blocks until cancelled, cancels the token mid-flight, and asserts the runner transitions the agent to `Cancelled` and stops emitting further backend events.
- **Effort:** M
- **Depends on:** Task 2.2

### Task 2.4 — `FakeBackend`
- **Files:** `crates/harness-runtime/src/testing/fake_backend.rs` (or a dedicated `harness-runtime` test-support module; consider `#[cfg(any(test, feature = "testing"))]` gating so it's reusable by later phases' integration tests without shipping in production builds)
- **Description:** `FakeBackend { scripted_events: Vec<ExecutionEvent> }` implementing `ExecutionBackend` (spec Section 68.2), streaming the scripted events through the provided `ExecutionEventSink`, honoring the `CancellationToken` (stop mid-script if cancelled), and returning a scripted `ExecutionResult`/`ExecutionError`.
- **Acceptance criteria:** implements the full `ExecutionBackend` trait from spec Section 11.1; used successfully by Tasks 2.2 and 2.3's tests.
- **Effort:** M
- **Depends on:** Task 1.5 (needs `ExecutionBackend`, `ExecutionEvent` types — note: `ExecutionBackend` trait itself must be defined; see Task 2.6)

### Task 2.5 — Fake tool registry
- **Files:** `crates/harness-runtime/src/testing/fake_tools.rs`
- **Description:** A `FakeToolExecutor` implementing `ToolExecutor` (spec Section 18) with deterministic scripted results/errors per call, plus a minimal `ToolRegistry` (spec Section 18) sufficient to route a `ToolRequest` to the right fake executor by `ToolId`.
- **Acceptance criteria:** `AgentRunner`, when it receives an `AgentEffect::ExecuteTool`, successfully calls through the fake registry and feeds `ToolCompleted`/`ToolFailed` back into the agent's mailbox.
- **Effort:** M
- **Depends on:** Task 1.3 (tool protocol types)

### Task 2.6 — `ExecutionBackend` and `ToolExecutor` trait definitions
- **Files:** `crates/harness-runtime/src/traits.rs` (or split: backend trait could live in `harness-model`/`harness-protocol` — **decision**: define `ExecutionBackend` in `harness-runtime` since spec Section 65 places `backend.rs` under `harness-protocol` for *types* like descriptors/capabilities, but the trait itself is a runtime-facing contract implemented by runtime-owned integration crates; keep `ExecutionBackend`/`ToolExecutor`/`Workspace` trait *definitions* in `harness-runtime`, re-exported for integration crates to implement against)
- **Description:** Define the actual `#[async_trait] pub trait ExecutionBackend` (spec Section 11.1) and `#[async_trait] pub trait ToolExecutor` (Section 18) here, using `async-trait` per the project's locked decision (`TASKS-00-OVERVIEW.md` §2) since both are used as `Arc<dyn ...>`.
- **Acceptance criteria:** both traits compile as `dyn`-compatible; `FakeBackend` (Task 2.4) and `FakeToolExecutor` (Task 2.5) both implement them without workarounds.
- **Effort:** M
- **Depends on:** Task 1.5, Task 1.3

### Task 2.7 — `SessionRuntime` and event bus
- **Files:** `crates/harness-runtime/src/runtime.rs` (or `session_runtime.rs`)
- **Description:** Implement `SessionRuntime` (spec Section 6.2: `state: SessionState`, `default_backend: Arc<dyn ExecutionBackend>`, `workspace: Arc<dyn Workspace>`, `event_bus: SessionEventBus`). `SessionEventBus` aggregates every child `AgentRunner`'s `broadcast::Sender<AgentEventEnvelope>` into one `SessionEvent` stream (spec Section 40/44), assigning `session_sequence`. For this phase, `workspace` can be a trivial in-memory fake (`FakeWorkspace`, spec Section 68.2) — real workspace implementations start in Phase 4.
- **Acceptance criteria:** subscribing to the session event bus and sending `SessionCommand::Prompt` produces a well-ordered `SessionEvent` stream including `SessionStarted`, wrapped `Agent` events, and `Completed`.
- **Effort:** L
- **Depends on:** Tasks 2.2, 2.6

### Task 2.8 — `SessionClient` / local session API
- **Files:** `crates/harness-runtime/src/handles.rs` (or `crates/harness-engine/src/session_builder.rs` if the public-facing type should live in `harness-engine` — **decision**: implement the concrete `LocalSessionClient` in `harness-runtime` in this phase since `harness-engine` doesn't exist as a builder surface until Phase 2 also needs `harness-engine` minimally stood up; see Task 2.9)
- **Description:** Implement `SessionClient`/`SessionApi` (spec Sections 48, 76): `send(SessionCommand)`, `snapshot()`, `subscribe()`. `snapshot()` requires an `AgentSnapshot`/`SessionSnapshot` projection (spec Section 45) built from the live `Agent`/`SessionRuntime` state — implement as a pure read of current state, not a stored copy.
- **Acceptance criteria:** `session.snapshot()` immediately after `session.send(Prompt)` (before completion) shows `status: WaitingForBackend` (or similar in-flight status); after completion shows `Completed` and populated usage.
- **Effort:** M
- **Depends on:** Task 2.7

### Task 2.9 — Minimal `harness-engine` session builder
- **Files:** `crates/harness-engine/src/session_builder.rs`, `crates/harness-engine/src/harness.rs`
- **Description:** Stand up just enough of the public API (spec Sections 63, 76) to construct a session in tests: `harness.session().backend(fake_backend).tools(fake_toolset).start().await?` returning a `SessionHandle` wrapping `LocalSessionClient` (Task 2.8). Full builder ergonomics (integration registry, workspace injection variety) expand in later phases — this task only needs to support the fake-backend, single-session case.
- **Acceptance criteria:** an integration test in `harness-engine` builds a session via this exact builder chain, sends a prompt, and asserts on the resulting event stream — matching the spec's Phase 2 goal statement verbatim.
- **Effort:** M
- **Depends on:** Task 2.8

### Task 2.10 — Replay-fixture harness (early version)
- **Files:** `crates/harness-runtime/tests/replay.rs`, `crates/harness-runtime/tests/fixtures/*.json`
- **Description:** Per spec Section 68.4: persist a command/event fixture (initial `AgentState` + ordered `AgentCommand` sequence + expected ordered `AgentEffect`/`AgentEvent` sequence) as JSON, and a test harness that replays it and asserts equality. This is lightweight in Phase 2 (no real persistence store yet — that's Phase 7) but establishes the pattern early so regressions are caught continuously.
- **Acceptance criteria:** at least 2 fixtures (happy-path run, cancelled run) committed and passing.
- **Effort:** M
- **Depends on:** Task 2.9

---

## Testing (this phase)

- Fake backend / fake tools (Tasks 2.4, 2.5) exercised throughout.
- Early replay tests (Task 2.10).
- No concurrency tests yet (single session only — Phase 5).

## Exit criteria

- `session.send(prompt)` and `session.subscribe()` work end-to-end against a fake backend (spec Section 71 Phase 2 goal, verified verbatim by Task 2.9's integration test).
- Cancellation of a single session's active run is demonstrated (Task 2.3).
- `harness-core` remains untouched by async/tokio; all async machinery lives in `harness-runtime`/`harness-engine`.

## Trade-offs / open decisions

- **Trait definition location:** `ExecutionBackend`/`ToolExecutor` defined in `harness-runtime` rather than `harness-protocol`, since they're behavioral contracts implemented by runtime-adjacent crates, not wire data. `harness-protocol` keeps only the data types they exchange (`ExecutionRequest`, `ExecutionEvent`, `ToolResult`, etc.). Document this clearly in each crate's `lib.rs` doc comment to avoid confusion later.
- **`Workspace` trait:** stubbed as `FakeWorkspace` here; the real trait definition should also live alongside `ExecutionBackend`/`ToolExecutor` (in `harness-runtime`, or promoted to its own already-scaffolded `harness-workspace` crate — recommend moving the trait *definition* into `harness-workspace` in Phase 4 when real implementations arrive, keeping `harness-runtime` dependent on `harness-workspace` rather than the reverse).

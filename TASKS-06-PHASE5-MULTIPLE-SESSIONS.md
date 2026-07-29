# Phase 5 — Multiple Sessions

**Goal (spec Section 71):** `SessionManager` runs many independent sessions concurrently, each with its own injected backend, coordinated by a global `Scheduler`; a session bound to Provider A and a session bound to Provider B stream concurrently without blocking each other.
**Depends on:** Phase 4 complete.
**Crates touched:** `harness-runtime`, `harness-engine`.

---

## Tasks

### Task 5.1 — `SessionManager`
- **Files:** `crates/harness-runtime/src/session_manager.rs`
- **Description:** Implement `SessionManager` (spec Section 33): `create_session`, `close_session`, `restore_session` (stub returning "not yet supported" until Phase 7), `session_handle(id)`. Holds a `HashMap<SessionId, SessionRuntime>` behind appropriate synchronization (e.g. `DashMap` or `RwLock<HashMap<..>>` — prefer `tokio::sync::RwLock` for consistency with the rest of the async stack unless contention profiling later says otherwise).
- **Acceptance criteria:** creating two sessions returns two distinct `SessionId`s with independent `SessionRuntime`s; closing one does not affect the other's handle validity.
- **Effort:** M
- **Depends on:** Phase 4 complete

### Task 5.2 — Per-session task isolation
- **Files:** `crates/harness-runtime/src/session_manager.rs`, `crates/harness-runtime/src/agent_runner.rs`
- **Description:** Each session's root `AgentRunner` (and any agents within it) runs on its own `tokio::spawn`ed task tree, independent of other sessions. Implement panic/failure isolation (spec Section 36): wrap each session's top-level task so a panic or unhandled error inside one session's execution is caught (`tokio::spawn` already isolates panics into a `JoinError`, but ensure `SessionManager` observes this and marks only that session `Failed` — does not propagate or crash the process).
- **Acceptance criteria:** a test deliberately panics inside one session's fake backend/tool; the other concurrently running session completes normally and the panicking session surfaces as `SessionStatus::Failed` with a captured error, not a process crash.
- **Effort:** M
- **Depends on:** Task 5.1

### Task 5.3 — Session-level event bus fan-out
- **Files:** `crates/harness-runtime/src/session_manager.rs`
- **Description:** Extend the Phase 2 `SessionEventBus` so `SessionManager` can multiplex many sessions' buses without cross-talk; verify `session_sequence` numbering stays independent per session (each session owns its own sequence counter, never shared globally).
- **Acceptance criteria:** subscribing to Session A never yields any Session B event, and vice versa, even when both stream concurrently in the same test.
- **Effort:** S
- **Depends on:** Task 5.1

### Task 5.4 — `Scheduler`
- **Files:** `crates/harness-runtime/src/scheduler.rs`
- **Description:** Implement `SchedulerConfig` and `Scheduler` (spec Section 34) using `tokio::sync::Semaphore` permits for: `max_active_sessions`, `max_active_agents`, `max_agents_per_session`, `max_concurrent_backend_requests`, `max_concurrent_tool_executions`, `max_concurrent_processes`. `SessionManager::create_session` acquires a session permit; `AgentRunner` acquires a backend-request permit before calling `ExecuteBackend` and a tool-execution permit before calling `ExecuteTool`. Use semaphores/permits, not unrestricted `tokio::spawn`, per the spec's explicit instruction.
- **Acceptance criteria:** with `max_concurrent_backend_requests = 1`, two sessions' simultaneous prompts are observed to serialize their backend calls (second one's `ExecuteBackend` effect visibly waits for the first's permit release) while everything else about each session proceeds independently.
- **Effort:** L
- **Depends on:** Task 5.1

### Task 5.5 — Backend-level rate limits
- **Files:** `crates/harness-runtime/src/scheduler.rs`
- **Description:** `BackendRateLimits` (spec Section 34: `max_concurrent_requests`, `requests_per_minute`, `tokens_per_minute`) applied per `BackendId`/`IntegrationId`, layered on top of the global `Scheduler` permits from Task 5.4 (i.e. a request must acquire both a global permit and a backend-specific permit/limiter).
- **Acceptance criteria:** configuring a low `requests_per_minute` for a fake backend and issuing rapid requests demonstrates throttling (requests queue rather than exceeding the configured rate).
- **Effort:** M
- **Depends on:** Task 5.4

### Task 5.6 — Per-session backend injection at scale
- **Files:** `crates/harness-engine/src/session_builder.rs`
- **Description:** Confirm/extend the builder from Phase 3 so `SessionManager`-created sessions each bind an independently injected `Arc<dyn ExecutionBackend>` with no shared global state between them (spec Section 12's exact example: Session A → Anthropic, Session B → a second backend instance, running concurrently).
- **Acceptance criteria:** the spec Section 71 Phase 5 required test passes verbatim: "Session A → Provider A, Session B → Provider B, both stream concurrently" (Provider B can be a second `FakeBackend` or `GenericModelBackend` instance if only Anthropic is implemented so far — the point under test is independence, not provider diversity).
- **Effort:** M
- **Depends on:** Tasks 5.1, 5.4

### Task 5.7 — Resource Manager (conflict coordination scaffold)
- **Files:** `crates/harness-runtime/src/resource_manager.rs`
- **Description:** Implement `ResourceKey` (`File`, `GitRepository`, `Workspace`, `Terminal`, `Custom`) and `AccessMode` (`Shared`/`Exclusive`) per spec Section 39, plus a minimal in-memory `ResourceManager` that tracks current holders and can grant/deny/queue access requests. Full workspace-conflict tool integration (e.g. wiring `fs.edit` to acquire an `Exclusive` lock on its target file) is a stretch goal for this phase — the required deliverable is that conflict management has an architectural home, per the spec's own wording ("not every implementation must use pessimistic locking, but conflict management must have an architectural home").
- **Acceptance criteria:** unit tests demonstrate `Shared` access can be held concurrently by multiple sessions, `Exclusive` access is refused while another exclusive holder is active, and released properly afterward.
- **Effort:** M
- **Depends on:** Task 5.1

### Task 5.8 — Concurrency test suite
- **Files:** `crates/harness-runtime/tests/concurrency.rs`
- **Description:** Per spec Section 68.5: two sessions streaming concurrently (Task 5.6), one session's cancellation does not cancel another (extends Phase 2 Task 2.3 to two sessions), child failure isolation (placeholder here, fully exercised in Phase 6), scheduler limits (Task 5.4/5.5), tool permission races (two agents requesting the same `Ask` tool concurrently, verifying no cross-agent leakage of permission state), workspace conflicts (Task 5.7).
- **Acceptance criteria:** all listed scenarios pass reliably under `cargo test -p harness-runtime -- --test-threads=1` and also under default parallel test execution (i.e. not flaky).
- **Effort:** L
- **Depends on:** Tasks 5.2, 5.4, 5.7

---

## Testing (this phase)

This phase is primarily about concurrency correctness — see Task 5.8, the first full application of the "Concurrency tests" bullet from `TASKS-00-OVERVIEW.md` §4.

## Exit criteria

- `SessionManager` creates/tracks/closes many concurrent sessions with independent backends (spec Section 71 Phase 5 goal, verified by Task 5.6).
- `Scheduler` enforces configured concurrency ceilings via semaphores, not unbounded spawning.
- One session's panic/failure never affects another (Task 5.2).
- `ResourceManager` exists as the architectural home for future conflict handling, even if only partially wired into tools at this stage.

## Trade-offs / open decisions

- **`ResourceManager` – wiring depth:** full pessimistic locking integration into every tool executor is explicitly deferred; only the coordination primitive itself is required now. Revisit wiring depth once real multi-session file-conflict scenarios are observed in practice (post-Phase 5).
- **`RwLock<HashMap<..>>` vs `DashMap`:** left as an implementation-time choice; start with `tokio::sync::RwLock<HashMap<..>>` for simplicity and swap to `DashMap` only if profiling shows lock contention under many concurrent sessions.

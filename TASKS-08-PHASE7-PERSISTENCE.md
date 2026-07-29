# Phase 7 — Persistence

**Goal (spec Section 71):** durable events, a SQLite store, session restore, transcript validation before persistence/backend submission, and snapshot + event restoration for reconnecting frontends.
**Depends on:** Phase 6 complete.
**Crates touched:** `harness-session-store`, `harness-runtime`, `harness-core` (transcript validation reuse).

**Locked decision reminder:** `rusqlite` + a single-writer actor task (not `sqlx`) is the chosen persistence approach (`TASKS-00-OVERVIEW.md` §2); JSONL remains available as a lighter alternative `SessionStore` implementation.

---

## Tasks

### Task 7.1 — `SessionStore` trait and durable event types
- **Files:** `crates/harness-session-store/src/store.rs`
- **Description:** Define `#[async_trait] pub trait SessionStore` (spec Section 59: `load_session`, `append`, `save_snapshot`) and `DurableSessionEvent`/`DurableSessionSnapshot`/`StoredSession` types. Implement the durable-vs-ephemeral split from spec Section 46: only message-completed, tool started/completed, agent spawned/completed, permission decisions, backend/model changes, usage records, errors, and relevant state transitions are ever wrapped as `DurableSessionEvent`s; raw text/reasoning/stdout deltas and progress ticks are never persisted individually (only the final assembled content is, as part of the message-completed event).
- **Acceptance criteria:** a filter function `fn is_durable(event: &AgentEvent) -> bool` has explicit test coverage for every `AgentEvent` variant, documenting the durable/ephemeral decision for each one (a table-driven test is ideal here so the decision is visible and reviewable).
- **Effort:** M
- **Depends on:** Phase 6 complete

### Task 7.2 — `rusqlite` schema and single-writer actor
- **Files:** `crates/harness-session-store/src/sqlite.rs`, `crates/harness-session-store/migrations/*.sql`
- **Description:** Add `rusqlite` (bundled feature for static SQLite) to this crate. Schema: `sessions`, `agents`, `durable_events` (append-only, indexed by `session_id, session_sequence`), `usage_records`, `snapshots`. Implement the single-writer actor pattern from the earlier research: one background task owns the single WAL-mode `rusqlite::Connection`, receiving write requests over an `mpsc` channel; reads may use a separate pooled connection (`r2d2_sqlite`/`deadpool-sqlite`) or a `spawn_blocking`-wrapped read-only connection, since SQLite serializes writers regardless of driver choice.
- **Acceptance criteria:** concurrent appends from two sessions never interleave/corrupt a single event row; `PRAGMA journal_mode=WAL` confirmed active; a crash-recovery test (kill the writer task mid-batch, reopen the DB) shows no partial/corrupt rows.
- **Effort:** L
- **Depends on:** Task 7.1

### Task 7.3 — `SqliteSessionStore` implementation
- **Files:** `crates/harness-session-store/src/sqlite.rs`
- **Description:** Implement `SessionStore` for the schema/actor from Task 7.2: `append` writes a `DurableSessionEvent` row, `save_snapshot` writes/replaces the latest `DurableSessionSnapshot` for a session, `load_session` reconstructs a `StoredSession` (latest snapshot + any durable events after it) sufficient to rebuild `SessionRuntime`/`Agent` state (Task 7.5).
- **Acceptance criteria:** round-trip test: append N events + a snapshot, reload, and confirm the reconstructed `StoredSession` matches exactly.
- **Effort:** M
- **Depends on:** Task 7.2

### Task 7.4 — `JsonlSessionStore` (lightweight alternative)
- **Files:** `crates/harness-session-store/src/jsonl.rs`
- **Description:** Implement `SessionStore` backed by append-only JSONL files (one per session, following this repo's own validated `.rusty/runs/*/events.jsonl` precedent): `OpenOptions::append(true)`, `serde_json::to_writer` + newline, routed through a single-writer task per session file, periodic `File::sync_data()` for durability. No indexed queries — `load_session` does a full sequential scan. Positioned as the default for the standalone/minimal build (spec Section 67 "Minimal/local" packaging) where SQLite may be undesirable.
- **Acceptance criteria:** same round-trip test as Task 7.3, run against this implementation, passes identically (shared `SessionStore` conformance test suite reused across both implementations — recommend building this as one shared test module parameterized over the two implementations).
- **Effort:** M
- **Depends on:** Task 7.1

### Task 7.5 — Session restore flow
- **Files:** `crates/harness-runtime/src/session_manager.rs`
- **Description:** Implement `SessionManager::restore_session` (stubbed in Phase 5) for real: `BackendReference` (Phase 1 Task 1.5) → `IntegrationRegistry` → `IntegrationFactory::create` → fresh `Arc<dyn ExecutionBackend>` → rebuild `SessionRuntime` from the `StoredSession` loaded via `SessionStore::load_session` (spec Section 15's restoration flow diagram, executed literally).
- **Acceptance criteria:** a session created, populated with a few turns, and then the process/`SessionManager` instance is dropped and recreated; `restore_session(id)` reconstructs an equivalent session whose `snapshot()` matches the pre-shutdown state (modulo ephemeral-only data, per Task 7.1's durable/ephemeral split).
- **Effort:** L
- **Depends on:** Tasks 7.3, 7.4

### Task 7.6 — Transcript validation before submission/persistence
- **Files:** `crates/harness-runtime/src/agent_runner.rs`
- **Description:** Call the Phase 1 Task 1.11 `validate_transcript` function at two chokepoints: (1) immediately before any `ExecuteBackend` effect is dispatched (catches an invalid transcript before it reaches a real provider, which may reject it with a confusing provider-specific error), and (2) immediately before any `Persist` effect is written via `SessionStore::append` (spec Section 60's "centralized validation" requirement).
- **Acceptance criteria:** a deliberately corrupted in-memory transcript (constructed only for the test, bypassing normal transitions) is caught at both chokepoints with a clear `HarnessError::Agent`/`TranscriptError`, never silently sent to a backend or persisted.
- **Effort:** M
- **Depends on:** Task 7.5

### Task 7.7 — `harness-engine` persistence wiring
- **Files:** `crates/harness-engine/src/builder.rs`
- **Description:** Add `.session_store(store)` to the `Harness::builder()` chain (spec Section 63's example) and `HarnessApi::restore_session` (spec Section 76). Default to an in-memory no-op store if none is configured, so Phases 1–6's tests continue to work unchanged without requiring persistence.
- **Acceptance criteria:** `Harness::builder().session_store(sqlite_store).build()` followed by `harness.restore_session(id)` works end-to-end against a real `SqliteSessionStore`.
- **Effort:** M
- **Depends on:** Task 7.5

### Task 7.8 — Replay-test hardening
- **Files:** `crates/harness-session-store/tests/replay.rs`
- **Description:** Extend the Phase 2 Task 2.10 replay-fixture pattern to use real persisted fixtures captured from actual failures/edge cases as they're discovered during this phase's development (spec Section 68.4: "persist command/event fixtures from real failures and replay them"). Establish the convention (fixture naming, storage location, how a new fixture is captured) so future phases keep contributing to this suite.
- **Acceptance criteria:** at least one fixture captured from a real bug found during Phase 7 development is committed and passing.
- **Effort:** S
- **Depends on:** Task 7.3

---

## Testing (this phase)

- Shared `SessionStore` conformance suite run against both `SqliteSessionStore` and `JsonlSessionStore` (Tasks 7.3/7.4).
- Crash-recovery test for the SQLite single-writer actor (Task 7.2).
- Restore-after-restart test (Task 7.5).
- Transcript-validation-at-chokepoint tests (Task 7.6).
- Hardened replay suite (Task 7.8).

## Exit criteria

- Both `SessionStore` implementations pass the same conformance suite.
- A session can be fully restored after a simulated process restart, with only ephemeral (never-durable) data lost.
- No invalid transcript ever reaches a backend or the store.
- Spec Section 71 Phase 7 checklist (durable events, SQLite store, session restore, transcript validation, snapshot+event restoration) is fully satisfied.

## Trade-offs / open decisions

- **Read-path concurrency for SQLite:** pooled read connections (`r2d2_sqlite`) vs. routing all reads through the same single-writer actor for simplicity at the cost of read/write contention — recommend starting with pooled reads since `load_session`/snapshot queries are latency-sensitive for restore UX, revisit only if WAL-mode contention proves a non-issue and the pool adds unwanted complexity.
- **Default store when unconfigured:** an in-memory no-op store keeps earlier phases' tests stable; confirm this doesn't mask a forgotten `.session_store(..)` call in a real deployment — consider a startup warning (via `tracing`) when persistence is not configured outside of test builds.

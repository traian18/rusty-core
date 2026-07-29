# Rust Agent Harness — Task Documents Index

**Source of truth:** `rust-agent-harness-development-spec.md` (referenced throughout as "the spec"; section numbers below map directly to it).
**Status:** Planning output. No implementation has started; repo is currently blank aside from the spec.

This index and the documents it links to translate the spec into an ordered, technical implementation backlog. Each phase document is self-contained: goal, crates touched, numbered tasks (with exact files, acceptance criteria, effort, and dependencies), phase-specific tests, and exit criteria.

---

## 1. Document set

| Doc | Phase | Content |
|---|---|---|
| `TASKS-01-PROJECT-SETUP.md` | Phase 0 | Repo scaffolding: workspace `Cargo.toml`, all crates/apps stubbed, CI, license, lint/format config, MSRV |
| `TASKS-02-PHASE1-CORE-VERTICAL-SLICE.md` | Phase 1 | IDs/protocol types, `Agent`, transcript, commands/effects, usage ledger, tool descriptor/capability types |
| `TASKS-03-PHASE2-SINGLE-SESSION-RUNTIME.md` | Phase 2 | `AgentRunner`, `SessionRuntime`, command/event channels, cancellation, fake backend/tools |
| `TASKS-04-PHASE3-GENERIC-MODEL-BACKEND.md` | Phase 3 | `ModelClient`, `GenericModelBackend`, first real provider (Anthropic), streaming, tool calls, usage |
| `TASKS-05-PHASE4-REAL-TOOLS.md` | Phase 4 | `fs.read`, `workspace.search`, `fs.edit`, `shell.exec` with capability checks, permission policy, cancellation |
| `TASKS-06-PHASE5-MULTIPLE-SESSIONS.md` | Phase 5 | `SessionManager`, independent session tasks, session event bus, scheduler, per-session backend injection |
| `TASKS-07-PHASE6-SUBAGENTS.md` | Phase 6 | `AgentSupervisor`, spawn/inheritance, budget, cancellation hierarchy, child completion/failure |
| `TASKS-08-PHASE7-PERSISTENCE.md` | Phase 7 | Durable events, SQLite store, session restore, transcript validation |
| `TASKS-09-PHASE8-STANDALONE-TUI.md` | Phase 8 | Terminal shell built only on public `Harness`/`SessionHandle` APIs |
| `TASKS-10-PHASE9-IDE-INTEGRATION.md` | Phase 9 | Sidecar (WebSocket) + native embedded modes, shared session semantics |
| `TASKS-11-PHASE10-SPECIALIZED-INTEGRATIONS.md` | Phase 10 | Claude Code and Codex behind `ExecutionBackend` |
| `TASKS-12-PHASE11-EXTENSION-SDK.md` | Phase 11 | Stable extension surface, subprocess plugins, MCP adapter (`rmcp`) |

Work through the docs in order — each phase's exit criteria are the entry criteria for the next.

---

## 2. Locked technical decisions (apply across all phases)

These were confirmed with the user or derived from bounded research and should not be re-litigated per-phase unless a phase doc explicitly flags a new decision:

- **Crate layout:** the full ~20+ crate workspace from spec Section 65 is scaffolded immediately (Phase 0), not grown incrementally.
- **Tooling:** this repo owns its own conventions (license, CI, lint config) — it is not deferring to a parent app.
- **First real backend (Phase 3):** Anthropic.
- **First tool set (Phase 4):** `fs.read`, `workspace.search`, `fs.edit`, `shell.exec` (matches spec Section 19 example exactly).
- **Async trait objects:** use `async-trait` for the public `dyn` traits (`ExecutionBackend`, `ToolExecutor`, `Workspace`, `ModelClient`, `ContextProvider`, `IntegrationFactory`, `SessionStore`). Native `async fn in trait` is reserved for internal, never-`dyn`-used traits only.
- **Cancellation:** `tokio_util::sync::CancellationToken`, one root token per session, `.child_token()` per agent/run/tool call, cascading cancellation per spec Section 35.
- **IDs:** `uuid` v4 (`uuid` crate, `v4` + `serde` features).
- **Money:** `rust_decimal` for `Cost.amount_usd` / `AgentBudget.max_cost_usd`.
- **Serialization:** `serde` + `serde_json` everywhere; `schemars` for `ToolDescriptor.input_schema` generation from Rust parameter structs.
- **Errors:** `thiserror` for every library crate's public error enums (`HarnessError` domains per spec Section 69); `anyhow` only inside `apps/*` binaries.
- **Logging vs events:** `tracing` (+ `tracing-subscriber` wired up only in `apps/*`) for internal diagnostics, kept strictly separate from the user-facing `AgentEvent`/`SessionEvent` protocol (spec Section 70).
- **Persistence (Phase 7):** `rusqlite` + a single-writer actor task (mpsc → one WAL-mode `Connection`), not `sqlx` — SQLite gains no real async concurrency benefit from sqlx, and rusqlite keeps the dependency tree and build times smaller. JSONL (this repo's own `.rusty/*.jsonl` pattern is a validated precedent) remains available as a lighter alternative `SessionStore` implementation.
- **Transports (Phase 9/10/11):** `tokio-tungstenite` for WebSocket (client+server), `jsonrpsee` for JSON-RPC, `rmcp` (official `modelcontextprotocol/rust-sdk`) for MCP — verify exact current versions on crates.io at implementation time.
- **License:** dual `MIT OR Apache-2.0` (ecosystem standard, patent-grant coverage).
- **Edition/MSRV:** Rust edition `2021`, `resolver = "2"`, `rust-version` pinned via `[workspace.package]` and mirrored in `rust-toolchain.toml`. See `TASKS-01-PROJECT-SETUP.md` for exact values and rationale.

## 3. Architectural invariants (enforce in every phase's code review)

Directly from spec Sections 66 and 72 — every task in every phase doc must be checked against these before merge:

1. `harness-core` and `harness-protocol` never depend on: Tauri, Ratatui, WebSocket, any provider SDK, SQLite, `reqwest`, or a concrete filesystem implementation.
2. Every session may bind a different `ExecutionBackend`; no global "active model/provider."
3. Tool *visibility* is an agent-capability concern (`AgentCapabilities.tools`); tool *registration* is a runtime concern (`ToolRegistry`). Registering a tool never grants it to an agent.
4. `ChildTools ⊆ ParentDelegatableTools` unless an explicit runtime/user policy override grants more.
5. Every meaningful execution stage emits a normalized `AgentEvent`/`SessionEvent`; nothing user-relevant happens silently.
6. Swapping transport (WebSocket ↔ direct in-process call) must not change session semantics — enforced by keeping `LocalSessionClient` and `RemoteSessionClient` behind one `SessionApi`.
7. One session's failure/panic must never take down another session or the runtime process.
8. `harness-core` performs no I/O and spawns no tasks; it only computes `AgentEffect`s from `AgentCommand`s.
9. Provider-specific request/event shapes terminate at the backend boundary (`GenericModelBackend`/`ModelClient` impls, or specialized backends) — never leak into `harness-core`.
10. Usage is recorded per execution (`UsageRecord`) and aggregated upward; unavailable metrics stay `None`, never coerced to `0`.

## 4. Testing strategy (cross-cutting, spec Section 68)

Each phase doc has a "Testing" subsection scoping this list to what's testable at that phase:

- **Core transition tests** (Phase 1+): pure `Agent::apply` unit tests, no Tokio runtime.
- **Fake backend / fake tools** (Phase 2+): `FakeBackend` with scripted `ExecutionEvent`s; deterministic scripted tool results.
- **Replay tests** (Phase 2+, hardened in Phase 7): persisted command/event fixtures replayed for regression coverage.
- **Concurrency tests** (Phase 5+): two sessions streaming concurrently; cancellation isolation; scheduler limits; tool permission races; workspace conflicts.
- **Backend contract tests** (Phase 3, extended Phase 10): one conformance suite (streaming ordering, cancellation, completion, usage behavior, tool-event normalization, error normalization) run against every `ExecutionBackend` impl.

## 5. Effort estimate legend (used in every phase doc)

| Symbol | Meaning |
|---|---|
| S | ~0.5–1 day |
| M | ~1–3 days |
| L | ~3–7 days |
| XL | > 1 week, should be decomposed further before starting |

Effort assumes one engineer already familiar with the spec and the previous phases' code.

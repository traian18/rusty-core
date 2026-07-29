# Phase 8 — Standalone TUI

**Goal (spec Section 71):** build a small terminal shell using only the public `Harness`/`SessionHandle` APIs, as an architectural validation step — the TUI must not depend on runtime internals.
**Depends on:** Phase 7 complete.
**Crates touched:** `apps/harness` (package `harness`). No changes to any `harness-*` library crate should be required; if this phase reveals a gap in the public API, that gap is the actual finding and must be fixed in `harness-engine`, not worked around in the TUI.

---

## Tasks

### Task 8.1 — Choose and wire a TUI rendering crate
- **Files:** `apps/harness/Cargo.toml`
- **Description:** Add a terminal UI crate (e.g. `ratatui` + `crossterm`) to `apps/harness` only — per spec Section 3's non-goals, the core/runtime/engine must never assume Ratatui. Confirm via the Phase 0 `xtask check-deps` guardrail that no `harness-core`/`harness-runtime`/`harness-engine` crate gains a Ratatui dependency.
- **Acceptance criteria:** `cargo run -p xtask -- check-deps` still passes after this addition.
- **Effort:** S
- **Depends on:** Phase 7 complete

### Task 8.2 — Harness construction and session lifecycle in the TUI
- **Files:** `apps/harness/src/harness_setup.rs`, `apps/harness/src/main.rs`
- **Description:** Construct `Harness::builder()` (spec Section 63) registering: the Anthropic integration (Phase 3), the four real tools (Phase 4), and a `SessionStore` (Phase 7 — default to the JSONL store for the minimal/local build per spec Section 67). Create one session per open "tab"/conversation the TUI supports.
- **Acceptance criteria:** starting the TUI, it builds a `Harness` and creates a session without any direct reference to `harness-runtime` or `harness-core` types (verified by `apps/harness`'s own dependency list only including `harness-engine`, `harness-protocol` (for shared display types), and its own tool/integration crates — not `harness-runtime`/`harness-core` directly).
- **Effort:** M
- **Depends on:** Task 8.1

### Task 8.3 — Event stream → terminal rendering
- **Files:** `apps/harness/src/render.rs`, `apps/harness/src/app_state.rs`
- **Description:** Subscribe to `session.subscribe()` and fold the `SessionEvent` stream into local TUI display state (spec Section 74's terminal rendering example: `● reading src/parser.rs`, `● running cargo test`, `● 2 subagents`, `> I found the issue...`). Combine with `session.snapshot()` on startup/reconnect to initialize state before the live stream catches up (spec Section 45).
- **Acceptance criteria:** manual verification: running a real (or fixture-driven) prompt shows live streaming text, at least one tool call, and completion in the terminal, matching the spec's example shape.
- **Effort:** L
- **Depends on:** Task 8.2

### Task 8.4 — Command input → `SessionCommand`
- **Files:** `apps/harness/src/input.rs`
- **Description:** Map terminal keyboard input to `SessionCommand::Prompt`, `CancelRun`, `ApprovePermission`/`RejectPermission` (for `Ask`-policy tools), and basic session switching if multiple sessions/tabs are supported.
- **Acceptance criteria:** pressing a cancel key mid-stream visibly stops the active run in the terminal; approving a permission prompt lets a blocked `shell.exec` proceed.
- **Effort:** M
- **Depends on:** Task 8.2

### Task 8.5 — Architectural validation checklist
- **Files:** none (review task)
- **Description:** Explicitly audit `apps/harness/Cargo.toml`'s dependency list against the invariant this phase exists to prove: the TUI depends only on `harness-engine` (+ whichever integration/tool crates it chooses to link, per spec Section 67's "an application chooses what to link") and never reaches into `harness-runtime`/`harness-core` internals, scheduler types, or backend-specific types after construction (spec Section 63's closing statement).
- **Acceptance criteria:** a documented pass/fail note recorded in this repo (e.g. appended to this file or a short `PHASE8-VALIDATION.md`) confirming the dependency audit; any violation found is fixed by adding the missing capability to `harness-engine`'s public API, not by reaching around it.
- **Effort:** S
- **Depends on:** Tasks 8.2–8.4

---

## Testing (this phase)

Mostly manual/exploratory (TUI rendering is hard to assert in an automated test), but retain: unit tests for the `SessionEvent → app_state` folding logic (Task 8.3) using scripted event sequences (reuse `FakeBackend` fixtures from earlier phases), and unit tests for input→command mapping (Task 8.4).

## Exit criteria

- A working terminal shell exists that can start a session, stream a response, show at least one tool call, handle a permission prompt, and cancel a run.
- The dependency audit (Task 8.5) confirms zero leakage of runtime/core internals into the TUI.
- Spec Section 71 Phase 8 goal ("architectural validation step") is explicitly satisfied and recorded.

## Trade-offs / open decisions

- **Multi-session TUI support:** full multi-tab terminal UX is not required for this phase's exit criteria (single active session is sufficient to prove the architecture); expand only if useful for later dogfooding.

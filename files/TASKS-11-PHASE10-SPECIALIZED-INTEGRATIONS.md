# Phase 10 — Specialized Integrations

**Goal (spec Section 71):** add Claude Code and Codex behind `ExecutionBackend`, validating that the abstraction is high-level enough for execution systems that own their own agent loop (not simple raw model APIs like Anthropic/OpenAI).
**Depends on:** Phase 9 complete.
**Crates touched:** `harness-integration-claude-code`, `harness-integration-codex`.

---

## Tasks

### Task 10.1 — Capability audit: what makes these backends different
- **Files:** none (design/analysis task, output captured as doc comments in Task 10.2/10.4)
- **Description:** Unlike `GenericModelBackend` (Phase 3), Claude Code and Codex may own part or all of their own agent loop, their own tool-calling semantics, and potentially their own permission/session-resumption model (spec Section 11: "others may own part or all of an agent loop"). Before writing code, explicitly enumerate for each: does it support `resumable_sessions`? `native_subagents`? `host_managed_tools` vs `backend_managed_tools`? `parallel_tool_calls`? `reasoning_stream`? This drives each backend's `BackendCapabilities` (spec Section 14) and prevents accidental identity-based branching (spec Section 14's explicit anti-pattern: `if backend.name() == "claude-code"`).
- **Acceptance criteria:** a short capability table (one row per backend) is written into each crate's module doc comment before implementation starts.
- **Effort:** S
- **Depends on:** Phase 9 complete

### Task 10.2 — Claude Code `ExecutionBackend`
- **Files:** `crates/integrations/claude-code/src/lib.rs`, `crates/integrations/claude-code/src/process.rs`, `crates/integrations/claude-code/src/protocol.rs`
- **Description:** Implement `ExecutionBackend` directly (not via `GenericModelBackend`, since Claude Code owns its own loop) by spawning/communicating with the Claude Code process/protocol. `harness-core`/`harness-runtime` must remain completely unaware of Claude Code's actual process protocol or permission syntax (spec Section 11's explicit list of things the core never knows). If Claude Code manages tools itself (`host_managed_tools`/`backend_managed_tools` distinction), decide and document how the harness's own `AgentToolset`/permission policy interacts with (or defers to) Claude Code's internal tool handling — this is the crux architectural question this phase is meant to answer.
- **Acceptance criteria:** `ClaudeCodeBackend` passes the Phase 3 Task 3.3 backend contract test suite (streaming ordering, cancellation, completion, usage behavior, tool-event normalization, error normalization), proving the same conformance bar applies regardless of how the backend is internally implemented.
- **Effort:** XL (recommend decomposing further once the real Claude Code process protocol is being integrated — exact subtasks depend on protocol details not fully specified here)
- **Depends on:** Task 10.1

### Task 10.3 — Claude Code session resumption (if supported)
- **Files:** `crates/integrations/claude-code/src/lib.rs`
- **Description:** If Claude Code exposes `resumable_sessions: true`, implement the corresponding `BackendReference`/restore-flow hook (spec Section 15) so Phase 7's `SessionStore`-based restore can, where applicable, also resume the backend's own native session state rather than only replaying the harness's transcript.
- **Acceptance criteria:** a restore test demonstrates the backend-native session resumes correctly (if the capability exists) or is cleanly skipped/no-op'd (if it doesn't), gated entirely by `BackendCapabilities.resumable_sessions`, never by a name check.
- **Effort:** L
- **Depends on:** Task 10.2

### Task 10.4 — Codex `ExecutionBackend`
- **Files:** `crates/integrations/codex/src/lib.rs`, `crates/integrations/codex/src/process.rs`, `crates/integrations/codex/src/protocol.rs`
- **Description:** Same shape as Task 10.2, targeting Codex's process/protocol instead. Reuse as much of the process-management scaffolding from `harness-integration-claude-code` as is reasonable (consider a small shared internal helper crate or module for "subprocess-protocol-backed `ExecutionBackend`" boilerplate if the two implementations converge structurally — evaluate after Task 10.2 is done, don't pre-abstract speculatively).
- **Acceptance criteria:** `CodexBackend` passes the same backend contract test suite as Task 10.2.
- **Effort:** XL (decompose further once Codex's actual protocol integration begins)
- **Depends on:** Task 10.1, ideally after Task 10.2 to reuse learnings

### Task 10.5 — Heterogeneous agent tree validation
- **Files:** `crates/harness-runtime/tests/heterogeneous_tree.rs`
- **Description:** Extend Phase 6's subagent test (Task 6.8) into the concrete scenario spec Section 24/73 describes: a root agent on one backend (e.g. Anthropic or Claude Code) spawning children on different backends (e.g. a Gemini-equivalent or Codex child), all coexisting in one agent tree, with usage records correctly tagging backend/model identity per record (spec Section 24: "Usage records therefore store backend/model identity per execution record").
- **Acceptance criteria:** the exact tree from spec Section 24's example (root + a differently-backed research child + a differently-backed implementation child) executes successfully with correct per-record backend/model attribution in the usage ledger.
- **Effort:** M
- **Depends on:** Tasks 10.2, 10.4

---

## Testing (this phase)

- Full backend contract suite (from Phase 3) run against both new backends — this is the primary correctness gate.
- Heterogeneous-tree integration test (Task 10.5).

## Exit criteria

- Both Claude Code and Codex backends exist behind `ExecutionBackend`, pass the shared contract suite, and required zero changes to `harness-core`/`harness-protocol`.
- No capability-identity branching (`if backend.name() == ...`) exists anywhere in `harness-runtime`/`harness-core`/`harness-engine`.
- A heterogeneous agent tree spanning at least three different backends executes correctly.

## Trade-offs / open decisions

- **Exact Claude Code / Codex process protocols:** this document intentionally does not assume specific wire details, since those are external, versioned protocols outside this spec's control — Tasks 10.2/10.4 are correctly sized as XL and **must** be broken into a dedicated sub-plan once the actual protocol/CLI surface is being integrated against.
- **Tool-handling deferral:** whether Claude Code/Codex's own tool execution should be exposed through the harness's `ToolRegistry`/permission policy, or treated as fully opaque to the harness (backend-managed), is the single most consequential open design question in this phase — resolve explicitly in Task 10.1's capability audit before writing backend code, not after.

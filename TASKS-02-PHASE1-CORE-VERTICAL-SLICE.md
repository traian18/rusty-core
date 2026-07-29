# Phase 1 — Core Vertical Slice

**Goal (spec Section 71):** a fake backend event flows through `Agent` and produces state + effects, with no real provider, no Tokio runtime, no I/O.
**Depends on:** Phase 0 complete.
**Crates touched:** `harness-protocol`, `harness-core`.

---

## Tasks

### Task 1.1 — ID and timestamp primitives
- **Files:** `crates/harness-protocol/src/ids.rs`, `crates/harness-protocol/src/lib.rs` (module wiring)
- **Description:** Newtype wrappers (per spec Section 6.2) for `SessionId`, `AgentId`, `RunId`, `RequestId`, `ToolCallId`, `MessageId`, `EventId`, `PermissionId`, `ToolId`, `IntegrationId`, `ConfigurationId`, `ModelId`, `BackendId`, `ContextProviderId`, all wrapping `Uuid` (`uuid` crate, `v4` feature) with `Serialize`/`Deserialize`/`Display`/`FromStr`/`Copy`/`Eq`/`Hash` derives via a small internal macro to avoid repetition. Add a `Timestamp` type wrapping `chrono::DateTime<Utc>` (add `chrono = { version = "0.4", features = ["serde"] }` to `[workspace.dependencies]` — not previously listed in Phase 0's dependency table, add it now).
- **Acceptance criteria:** each ID type round-trips through `serde_json`; `Timestamp::now()` helper exists; unit tests cover `Display`/`FromStr` round-trip.
- **Effort:** M
- **Depends on:** none (first Phase 1 task)

### Task 1.2 — Messages and transcript protocol types
- **Files:** `crates/harness-protocol/src/messages.rs`
- **Description:** `AgentMessage`, `MessageRole` (System/User/Assistant/Tool), and a content-block enum (`Text`, `ToolUse { call: ToolCall }`, `ToolResult { call_id: ToolCallId, result: ToolResultSummary }`, `Image { .. }` placeholder) per spec Sections 7–8 and 60. Keep this intentionally provider-agnostic — no Anthropic/OpenAI-shaped fields.
- **Acceptance criteria:** serializes/deserializes via `serde_json`; matches the transcript invariant described in spec Section 60 (`assistant(tool_call) → tool_result → next message`) structurally (i.e. the types make it possible to express and later validate this).
- **Effort:** M
- **Depends on:** Task 1.1

### Task 1.3 — Tool protocol types
- **Files:** `crates/harness-protocol/src/tools.rs`
- **Description:** `ToolDescriptor` (`id`, `name`, `description`, `input_schema: schemars::schema::RootSchema` or a serde-friendly JSON-Schema wrapper), `ToolPolicy`, `PermissionMode`, `ToolCapability`, `AgentToolset` (with an `enabled_descriptors()` method per spec Section 19), `ToolCall`, `ToolResult`, `ToolResultSummary`, `ToolError`, `ToolProgress`. Add `schemars` to this crate's dependencies (already in workspace table from Phase 0).
- **Acceptance criteria:** `AgentToolset::enabled_descriptors()` returns only tools with `ToolPolicy.enabled == true`; unit test constructs a toolset with a disabled tool and asserts it's excluded.
- **Effort:** M
- **Depends on:** Task 1.1

### Task 1.4 — Usage, cost, and budget protocol types
- **Files:** `crates/harness-protocol/src/usage.rs`
- **Description:** `ModelUsage` (all fields `Option<u64>` — spec Section 27, "unknown must not mean zero"), `UsageRecord`, `Cost`/`CostSource` (`rust_decimal::Decimal`), `AgentBudget`, `CumulativeUsage`/`ContextUsage`/`RunUsage`/`AgentUsageMetrics` (spec Section 28), `AgentUsageSummary` (self/descendant/inclusive — Section 31), `AgentUsageSnapshot`, `SessionUsageSnapshot`.
- **Acceptance criteria:** unit test constructs a `ModelUsage` with `input_tokens: None` and asserts any aggregation helper treats it as "unknown", not `0`, when combined with a `Some(0)` value from another record (i.e. `None + Some(0)` must stay distinguishable from `Some(0) + Some(0)` — implement aggregation as `Option<u64>` that only turns `Some` once at least one contributing record reports `Some`).
- **Effort:** M
- **Depends on:** Task 1.1

### Task 1.5 — Backend protocol types
- **Files:** `crates/harness-protocol/src/backend.rs`
- **Description:** `BackendDescriptor`, `BackendCapabilities` (spec Section 14, all booleans), `BackendReference` (spec Section 15), `BackendBinding`, `ExecutionRequest`, `ExecutionContext`, `ExecutionResult`, `ExecutionError`, and the normalized `ExecutionEvent` enum that a backend streams and that `AgentCommand::BackendEvent` wraps (text delta, reasoning delta, tool-call request, usage update, completion, error — mirrors the shape of `AgentEvent` in Task 1.7 but is the *input* normalized form from a backend, not the *output* agent event).
- **Acceptance criteria:** `BackendCapabilities` has one field per capability listed in spec Section 14; a unit test demonstrates capability-based branching (`if caps.resumable_sessions { .. }`) compiles and works without any string/name comparison.
- **Effort:** M
- **Depends on:** Task 1.1

### Task 1.6 — Commands, effects, operation, status
- **Files:** `crates/harness-protocol/src/commands.rs`, `crates/harness-protocol/src/effects.rs` (or both re-exported from `harness-core` if preferred — recommendation: keep `AgentCommand`/`AgentEffect` in `harness-protocol` since they are wire-shaped/serializable contracts, per spec Section 65's file layout which lists them under `harness-core`; **decision needed**: spec Section 65 places `effects.rs` under `harness-core`, but Section 9 frames commands/effects as the core transition contract. Resolve by keeping `AgentCommand`/`AgentEffect`/`AgentOperation`/`AgentStatus` enums themselves in `harness-protocol` (pure data, serializable, shared with any future remote transport) and let `harness-core` own only the `Agent::apply` behavior that consumes/produces them. Flag this as a documented deviation from Section 65's literal file list.)
- **Description:** `AgentCommand` (spec Section 9.1), `AgentEffect` (Section 9.2), `AgentOperation` (Section 8), `AgentStatus` (Section 8), `SpawnAgentSpec`/`BackendPolicy`/`ToolInheritance`/`WorkspacePolicy`/`SpawnMode` (Section 22), `PermissionRequest`/`PermissionDecision` (Section 61).
- **Acceptance criteria:** all enums derive `Serialize`/`Deserialize`/`Debug`/`Clone`; `ToolInheritance::Subset` and `::Replace` variants compile against `AgentToolset` from Task 1.3.
- **Effort:** M
- **Depends on:** Tasks 1.2, 1.3, 1.5

### Task 1.7 — Agent events and envelope
- **Files:** `crates/harness-protocol/src/events.rs`
- **Description:** `AgentEvent` (spec Section 41, all variants), `AgentEventEnvelope` (Section 43, with `agent_sequence`/`session_sequence` monotonic counters — note in a doc comment: "do not rely on timestamps alone for ordering," per spec), `EventVisibility` (Section 47).
- **Acceptance criteria:** `AgentEventEnvelope` serializes with a stable field order; a unit test asserts two envelopes constructed with increasing `agent_sequence` sort correctly even with identical timestamps.
- **Effort:** M
- **Depends on:** Task 1.6

### Task 1.8 — `Agent` and `AgentState` (harness-core)
- **Files:** `crates/harness-core/src/agent.rs`, `crates/harness-core/src/agent_state.rs`
- **Description:** `Agent` struct (spec Section 7.2: `id`, `session_id`, `parent_id`, `state`, `backend: BackendBinding`, `capabilities: AgentCapabilities`, `usage: UsageLedger`, `budget: AgentBudget`) and `AgentState` (Section 8: `status`, `system_prompt`, `messages`, `active_run`, `pending_tools: HashMap<ToolCallId, PendingToolCall>`, `children`, `last_error`). `harness-core` depends on `harness-protocol` only.
- **Acceptance criteria:** struct compiles with all fields; constructing a default/new `Agent` for a root agent (no parent) and a child `Agent` (with `parent_id`) both succeed via a small constructor API.
- **Effort:** M
- **Depends on:** Task 1.7

### Task 1.9 — Agent capabilities and budget logic
- **Files:** `crates/harness-core/src/capabilities.rs`, `crates/harness-core/src/budget.rs`
- **Description:** `AgentCapabilities` (spec Section 20). Logic: `can_delegate(tool_id) -> bool` (checks `ToolCapability.delegatable`), a `derive_child_capabilities(&self, inheritance: &ToolInheritance) -> Result<AgentToolset, CapabilityError>` implementing the non-escalation invariant from spec Section 23 (`ChildTools ⊆ ParentDelegatableTools`, rejecting any `Subset`/`Replace` request that includes a non-delegatable or absent tool). Budget: helper methods to check whether a proposed usage/cost would exceed `AgentBudget` limits (used later by the runtime, but the *check* itself is deterministic core logic).
- **Acceptance criteria:** unit test: parent has `fs.read` (delegatable) and `shell.exec` (not delegatable); requesting `ToolInheritance::Subset([fs.read, shell.exec])` for a child is rejected with a clear error; requesting `Subset([fs.read])` succeeds.
- **Effort:** M
- **Depends on:** Task 1.8

### Task 1.10 — Usage ledger aggregation
- **Files:** `crates/harness-core/src/usage.rs` (or extend `agent.rs`)
- **Description:** `UsageLedger` (Section 26) plus aggregation functions producing `AgentUsageSummary` (self/descendant/inclusive, Section 31) from a tree of ledgers **without mutating a parent's direct usage when a child spends** (explicit spec requirement in Section 31). Also implement the cumulative-vs-context-vs-run distinction (Section 28) as a pure function over a `Vec<UsageRecord>`.
- **Acceptance criteria:** unit test: parent has 2 self records, one child has 1 record; `inclusive_usage` sums all three, `self_usage` only the parent's 2, `descendant_usage` only the child's 1; no double counting when computed twice.
- **Effort:** M
- **Depends on:** Task 1.8

### Task 1.11 — Transcript invariant validation
- **Files:** `crates/harness-core/src/transcript.rs`
- **Description:** A pure function `validate_transcript(messages: &[AgentMessage]) -> Result<(), TranscriptError>` enforcing spec Section 60: every `assistant(tool_call)` must be followed by a matching `tool_result` before the next conversational message; no unresolved dangling tool calls unless explicitly allowed. This is invoked by the transition logic (Task 1.12) before accepting certain commands and reused again in Phase 7 before backend submission/persistence.
- **Acceptance criteria:** unit tests: valid transcript passes; a transcript with a dangling unresolved tool call fails with a specific `TranscriptError` variant naming the offending `ToolCallId`.
- **Effort:** M
- **Depends on:** Task 1.8

### Task 1.12 — `Agent::apply` transition function
- **Files:** `crates/harness-core/src/transitions.rs`
- **Description:** Implement `impl Agent { pub fn apply(&mut self, command: AgentCommand) -> Vec<AgentEffect> }` (spec Section 9) covering every `AgentCommand` variant:
  - `StartRun` → transitions to `PreparingContext`/`WaitingForBackend`, emits `ExecuteBackend` + `Emit(RunStarted)` effects.
  - `BackendEvent` → updates transcript/status per event kind, may emit `ExecuteTool`, `RequestPermission`, `Emit(AssistantTextDelta/...)`, `FinishRun`.
  - `ToolCompleted`/`ToolFailed` → removes from `pending_tools`, appends `tool_result` message, emits `Emit(ToolCallCompleted)`, may re-enter `WaitingForBackend`.
  - `PermissionResolved` → either proceeds to `ExecuteTool` or records denial and continues the run.
  - `ChildCompleted`/`ChildFailed` → updates `children`/status, emits `Emit(ChildAgentCompleted)`.
  - `Cancel`/`Pause`/`Resume` → status transitions, `Cancel` also emits effects to cancel any in-flight `ExecuteBackend`/`ExecuteTool`/`SpawnAgent` (the *emission*, not execution — execution is a Phase 2 runtime concern).
  This function must not perform I/O, must not spawn tasks, and must be fully deterministic given the same `(AgentState, AgentCommand)` pair (spec Section 72, "deterministic core").
- **Acceptance criteria:** covers the full `AgentCommand` enum with at least one test per variant; a scripted sequence test (`StartRun` → `BackendEvent(ToolCallRequested)` → `ToolCompleted` → `BackendEvent(Completed)`) produces the exact expected ordered `Vec<AgentEffect>` and final `AgentState`.
- **Effort:** L
- **Depends on:** Tasks 1.9, 1.10, 1.11

### Task 1.13 — Core transition test suite
- **Files:** `crates/harness-core/tests/transitions.rs` (or `#[cfg(test)]` modules alongside `transitions.rs`)
- **Description:** Per spec Section 68.1: no network, no Tokio runtime, no real tool. At minimum: `tool_call_emits_execute_effect`, `cancel_stops_further_effects`, `permission_ask_blocks_until_resolved`, `child_failure_does_not_crash_parent_state`, `non_delegatable_tool_rejected_for_child` (reuses Task 1.9), `unknown_usage_stays_none_through_a_full_run` (reuses Task 1.10).
- **Acceptance criteria:** `cargo test -p harness-core` passes; test file requires zero async/tokio dependencies (verified by `harness-core`'s `Cargo.toml` having no `tokio` dependency at all in this phase).
- **Effort:** M
- **Depends on:** Task 1.12

---

## Testing (this phase)

Only "core transition tests" from `TASKS-00-OVERVIEW.md` §4 apply here — see Task 1.13. Fake backend/runtime-level tests begin in Phase 2.

## Exit criteria

- `harness-core` has zero I/O, zero async runtime dependency, and zero task-spawning.
- A scripted fake `ExecutionEvent` sequence, fed through `AgentCommand::BackendEvent`, produces the exact expected `AgentState` + `Vec<AgentEffect>` (spec Section 71 Phase 1 goal, verified by Task 1.12's scripted test).
- Non-escalation invariant (`ChildTools ⊆ ParentDelegatableTools`) is enforced and tested.
- Usage "unknown is not zero" invariant is enforced and tested.

## Trade-offs / open decisions

- **Where `AgentCommand`/`AgentEffect` live:** this doc places them in `harness-protocol` (wire-shaped, serializable, reusable by a future remote transport) rather than `harness-core` as spec Section 65's literal file list suggests. `harness-core` owns only the behavior (`Agent::apply`). Revisit if this creates awkward re-export patterns once Phase 9/10 transports are built.
- **Timestamp backing type:** `chrono` chosen over `time` crate for ubiquity/ergonomics; either is acceptable, this is not architecturally significant and can be swapped without touching other phases if `chrono`'s maintenance status changes.

# Phase 6 — Subagents

**Goal (spec Section 71):** a root agent spawns two children that execute concurrently, backend/tool/workspace/budget inheritance resolved correctly, root receives both results.
**Depends on:** Phase 5 complete.
**Crates touched:** `harness-runtime`, `harness-core` (capability/budget helpers extended if needed), `harness-workspace` (worktree policy).

---

## Tasks

### Task 6.1 — `AgentSupervisor`
- **Files:** `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** Implement `AgentSupervisor` (spec Section 33): one per session, tracks parent/child `AgentId` relationships, spawns child `AgentRunner` tasks, propagates cancellation (extends Phase 2/5's `CancellationToken` hierarchy — spec Section 35's exact tree: session token → root token → child tokens → grandchild tokens), isolates child failures (does not crash the parent's task), cleans up completed agents, enforces `max_children`/`max_depth` from `AgentBudget` (Phase 1 Task 1.4).
- **Acceptance criteria:** spawning a child beyond `max_children` or beyond `max_depth` is rejected with a clear error before any child task is created.
- **Effort:** L
- **Depends on:** Phase 5 complete

### Task 6.2 — `SpawnAgent` effect interpretation
- **Files:** `crates/harness-runtime/src/agent_runner.rs`, `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** Implement the full spawn flow from spec Section 21: `AgentEffect::SpawnAgent { spec: SpawnAgentSpec }` → `AgentSupervisor` (1) creates a new `AgentId`, (2) resolves `BackendPolicy` (`Inherit` copies the parent's `BackendBinding`; `Explicit` resolves a new one via `IntegrationRegistry`, wired properly once Task 6.3 lands), (3) resolves `ToolInheritance` using `Agent::derive_child_capabilities` (Phase 1 Task 1.9 — enforces non-escalation), (4) resolves `WorkspacePolicy` (Task 6.5), (5) applies the spawn spec's `AgentBudget` (which may be tighter than the parent's, never looser without explicit override), (6) creates a child `CancellationToken` via `.child_token()`, (7) registers the parent/child relationship, (8) spawns the child `AgentRunner`.
- **Acceptance criteria:** an integration test spawns a child with `ToolInheritance::Subset([fs.read])` from a parent that has `fs.read` (delegatable) and `shell.exec` (not delegatable); the child's resulting `AgentCapabilities` contains only `fs.read`.
- **Effort:** L
- **Depends on:** Task 6.1

### Task 6.3 — Backend inheritance/override resolution
- **Files:** `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** For `BackendPolicy::Explicit(BackendReference)`, resolve through the `IntegrationRegistry` (spec Section 15/16, expected to exist from Phase 3's `AnthropicBackend` `IntegrationFactory` registration) to produce a fresh `Arc<dyn ExecutionBackend>` for the child, independent of the parent's. This is the mechanism that enables heterogeneous agent trees (spec Section 24: Claude root, Gemini/Codex children).
- **Acceptance criteria:** a child spawned with an explicit backend reference to a second registered fake/real backend actually executes against that backend, not the parent's.
- **Effort:** M
- **Depends on:** Task 6.2

### Task 6.4 — `SpawnMode` (await vs concurrent)
- **Files:** `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** `SpawnMode::AwaitResult` — parent's status becomes `WaitingForChildren` and it does not proceed until the spawned child(ren) complete/fail. `SpawnMode::Concurrent` — parent continues its own execution while the child runs in the background, receiving `ChildCompleted`/`ChildFailed` asynchronously via its mailbox whenever they arrive.
- **Acceptance criteria:** a test spawning two children with `AwaitResult` demonstrates the parent genuinely blocks (no further parent-driven effects until both children resolve); a separate test with `Concurrent` demonstrates the parent keeps processing its own commands while children run.
- **Effort:** M
- **Depends on:** Task 6.2

### Task 6.5 — Workspace policy resolution for children
- **Files:** `crates/harness-workspace/src/worktree.rs`, `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** Implement `WorkspacePolicy` resolution (spec Section 22/38): `Inherit` (share parent's `Workspace` handle), `ReadOnly` (wrap the parent's workspace in a `ReadOnlyWorkspace` adapter that fails all `write` calls with a clear `WorkspaceError`), `Snapshot` (copy current file state into an isolated temp location before handing it to the child), `NewWorktree` (create a real `git worktree add` checkout, implemented in `harness-workspace/src/worktree.rs`, and bind the child to a `FsWorkspace` rooted there).
- **Acceptance criteria:** each of the four policies has a dedicated test: `ReadOnly` rejects a write attempt; `Snapshot` isolates a child's edits from the parent's live files; `NewWorktree` produces a real, independent git worktree whose commits don't appear on the parent's checked-out branch until explicitly merged.
- **Effort:** L
- **Depends on:** Task 6.2

### Task 6.6 — Child completion/failure propagation
- **Files:** `crates/harness-runtime/src/agent_supervisor.rs`, `crates/harness-core/src/transitions.rs` (verify existing `ChildCompleted`/`ChildFailed` handling from Phase 1 is sufficient; extend if gaps found)
- **Description:** Wire real child lifecycle events into the parent's mailbox as `AgentCommand::ChildCompleted`/`ChildFailed` (spec Sections 9.1, 36), and ensure `AgentEvent::ChildAgentSpawned`/`ChildAgentCompleted` are emitted on the session event bus so the IDE sees subagent activity per spec Section 73's example trace.
- **Acceptance criteria:** the session event stream, when a root spawns a child that completes, shows the exact ordered events from spec Section 73: `AgentAdded A1` → status updates → tool activity → `A1 completed` → `A usage updated`.
- **Effort:** M
- **Depends on:** Tasks 6.2, 6.4

### Task 6.7 — Usage aggregation across the agent tree (real wiring)
- **Files:** `crates/harness-runtime/src/agent_supervisor.rs`
- **Description:** Wire the Phase 1 Task 1.10 aggregation logic (`self`/`descendant`/`inclusive` usage) into the real running tree: when a child completes, its usage becomes queryable as the parent's `descendant_usage` without mutating the parent's own `self_usage` ledger (spec Section 31's explicit invariant).
- **Acceptance criteria:** after two children each report usage, the root's `AgentUsageSummary.inclusive_usage` exactly equals `self_usage + sum(children's self_usage)`, verified numerically in a test.
- **Effort:** M
- **Depends on:** Task 6.6

### Task 6.8 — Required Phase 6 concurrency test
- **Files:** `crates/harness-runtime/tests/subagents.rs`
- **Description:** Implement the exact test required by spec Section 71: "root spawns two children, children execute concurrently, root receives results." Extend with a heterogeneous-backend variant per spec Section 24 (root on Backend A, one child on Backend B) if Task 6.3's second backend is available.
- **Acceptance criteria:** test passes deterministically; timing assertions (if any) tolerate CI scheduling jitter (avoid brittle sleep-based assertions — prefer explicit synchronization via channels/events).
- **Effort:** M
- **Depends on:** Tasks 6.4, 6.6, 6.7

---

## Testing (this phase)

Extends the Phase 5 concurrency suite with the subagent-specific scenario (Task 6.8) plus dedicated coverage for capability non-escalation (Task 6.2), workspace policy isolation (Task 6.5), and usage aggregation correctness (Task 6.7).

## Exit criteria

- Root agent can spawn subagents with `Inherit`/`Explicit` backend policy, `InheritAll`/`Subset`/`Replace` tool inheritance (with non-escalation enforced), and all four workspace policies.
- `AwaitResult` and `Concurrent` spawn modes both work correctly.
- Child failure never crashes the parent; child usage aggregates upward without double counting or parent-ledger mutation.
- The exact spec Section 71 Phase 6 required test (two concurrent children, root receives results) passes.

## Trade-offs / open decisions

- **`Snapshot` implementation:** could be a simple recursive file copy (simplest, chosen default) or a copy-on-write filesystem trick (e.g. reflinks where supported) — start with plain copy, optimize later only if snapshot creation latency becomes a real bottleneck.
- **`NewWorktree` cleanup policy:** decide (and document in code) whether worktrees are deleted automatically on child completion or retained for post-hoc inspection/debugging; recommend retaining by default with an explicit cleanup command, since debugging subagent work is a common need.

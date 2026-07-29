# Phase 4 — Real Tools

**Goal (spec Section 71):** implement `fs.read`, `workspace.search`, `fs.edit`, `shell.exec` with real capability checks, permission policy, cancellation, and tool events — replacing the fake tool registry for real usage.
**Depends on:** Phase 3 complete (can run in parallel with Phase 3 in practice since tools don't depend on the model backend, but is sequenced after it here per the spec's own phase ordering).
**Crates touched:** `harness-workspace`, `harness-tools`, `harness-tool-filesystem`, `harness-tool-shell`, `harness-tool-git` (optional stretch), `harness-runtime` (permission flow wiring).

---

## Tasks

### Task 4.1 — Real `Workspace` trait and `FsWorkspace`
- **Files:** `crates/harness-workspace/src/workspace.rs`, `crates/harness-workspace/src/filesystem.rs`
- **Description:** Move the `Workspace` trait definition (spec Section 37) from its Phase 2 stub location into `harness-workspace` (per the Phase 2 doc's flagged deferral). Implement `FsWorkspace` backed by real `tokio::fs` I/O: `read`, `write`, `search` (a simple recursive text search sufficient for this phase — richer indexing is out of scope), `status`.
- **Acceptance criteria:** `FsWorkspace` passes a shared `Workspace` conformance test suite (read/write round trip, search returns expected matches, status reflects reality) against a temp directory.
- **Effort:** M
- **Depends on:** Phase 3 complete

### Task 4.2 — `WorkspaceMode` and isolation scaffolding
- **Files:** `crates/harness-workspace/src/workspace.rs`
- **Description:** `WorkspaceMode` (`Shared`/`Isolated`, spec Section 38). For this phase, implement `Shared` fully (multiple sessions point at the same `FsWorkspace` root) and stub `Isolated` (worktree-backed) as a documented not-yet-implemented path returning a clear `WorkspaceError` — full worktree support is not required until subagent workspace policies in Phase 6.
- **Acceptance criteria:** two `FsWorkspace` handles pointed at the same root observe each other's writes (Shared mode demonstrated in a test).
- **Effort:** S
- **Depends on:** Task 4.1

### Task 4.3 — `ToolRegistry` (real)
- **Files:** `crates/harness-tools/src/registry.rs`, `crates/harness-tools/src/executor.rs`
- **Description:** Promote the fake registry pattern from Phase 2 into the real `ToolRegistry` (spec Section 18): `executors: HashMap<ToolId, Arc<dyn ToolExecutor>>`, registration API used by `harness-engine`'s builder (`register_tool`, spec Section 63).
- **Acceptance criteria:** registering two tools and resolving by `ToolId` works; resolving an unregistered `ToolId` returns a clear error (used by the capability-check flow in Task 4.6).
- **Effort:** S
- **Depends on:** Phase 2 complete

### Task 4.4 — `harness-tool-filesystem`: `fs.read`, `fs.edit`
- **Files:** `crates/tools/filesystem/src/lib.rs`, `crates/tools/filesystem/src/read.rs`, `crates/tools/filesystem/src/edit.rs`
- **Description:** `ToolExecutor` implementations for `fs.read` (delegates to the session's injected `Workspace::read`) and `fs.edit` (delegates to `Workspace::write`, with a diff/patch input shape — decide at implementation time whether `fs.edit` takes a full replacement or a patch format; recommend starting with whole-file replacement for simplicity, with patch-based editing as a fast-follow). Each executor's `descriptor()` provides a `schemars`-generated `input_schema`.
- **Acceptance criteria:** executing `fs.read` against a real `FsWorkspace` returns file contents; executing `fs.edit` writes and is reflected on a subsequent `fs.read`.
- **Effort:** M
- **Depends on:** Tasks 4.1, 4.3

### Task 4.5 — `harness-tool-filesystem`: `workspace.search`
- **Files:** `crates/tools/filesystem/src/search.rs`
- **Description:** `ToolExecutor` for `workspace.search`, delegating to `Workspace::search` (Task 4.1), normalizing results into `ToolResult`/`SearchResult`.
- **Acceptance criteria:** searching for a known string in a fixture workspace returns the expected matches with file/line info.
- **Effort:** S
- **Depends on:** Task 4.1, 4.3

### Task 4.6 — `harness-tool-shell`: `shell.exec`
- **Files:** `crates/tools/shell/src/lib.rs`, `crates/tools/shell/src/executor.rs`
- **Description:** `ToolExecutor` for `shell.exec` using `tokio::process::Command`, streaming stdout/stderr as `ToolProgress` events (spec Section 41 `ToolCallProgress`), honoring the `CancellationToken` (kill the child process on cancellation — spec Section 35's "tools/processes" leaf of the cancellation cascade), and enforcing the scheduler's `max_concurrent_processes` limit is respected by the caller (the executor itself just needs to be well-behaved; the actual semaphore lives in Phase 5's `Scheduler`).
- **Acceptance criteria:** running a short-lived command returns expected stdout in the `ToolResult`; running a long-lived command and cancelling mid-flight results in the child process being killed (verified via a process-exists check) and a `ToolError::Cancelled`-shaped result.
- **Effort:** L
- **Depends on:** Task 4.3

### Task 4.7 — Capability check → permission policy → executor flow
- **Files:** `crates/harness-runtime/src/agent_runner.rs` (extend Phase 2's effect interpreter), `crates/harness-runtime/src/permissions.rs`
- **Description:** Implement the full flow from spec Section 18/61: model requests a tool → `AgentRunner` checks `agent.capabilities.tools` contains and enables the tool → checks `ToolPolicy.permission` (`Allow`/`Ask`/`Deny`) → if `Ask`, emits `AgentEffect::RequestPermission` and suspends (agent status `WaitingForPermission`) until `SessionCommand::ApprovePermission`/`RejectPermission` arrives → if `Deny`, immediately synthesizes a `ToolFailed` command without ever reaching the registry → if `Allow` (or resolved to allow), dispatches to `ToolRegistry`.
- **Acceptance criteria:** three integration tests: (a) `Allow` tool executes immediately; (b) `Ask` tool blocks until `ApprovePermission`, then executes; (c) `Deny` tool never reaches the fake/real executor and the agent receives a `ToolFailed` with a policy-denial error.
- **Effort:** L
- **Depends on:** Task 4.3, Phase 1 Task 1.6 (permission types)

### Task 4.8 — Tool injection at session creation
- **Files:** `crates/harness-engine/src/session_builder.rs`
- **Description:** Implement the `.tools([...])` builder call exactly as spec Section 19's example (`ToolCapability::allow("fs.read")`, `::ask("shell.exec")`, `::deny("git.push")`), wiring the resulting `AgentToolset` into the root `Agent`'s `AgentCapabilities` at construction time. Confirm the model-facing tool list is always obtained via `agent.capabilities.tools.enabled_descriptors()` (Task 1.3) and never from the full `ToolRegistry` (spec Section 19's explicit warning: "never advertise the complete runtime tool registry to every agent").
- **Acceptance criteria:** a session created with only `fs.read` allowed never sees `shell.exec` in its model-facing tool definitions, even though `shell.exec` is registered in the runtime's `ToolRegistry`.
- **Effort:** M
- **Depends on:** Task 4.7

### Task 4.9 — End-to-end real-tools session test
- **Files:** `crates/harness-engine/tests/real_tools_e2e.rs`
- **Description:** Full session (fake or Anthropic backend, either is acceptable here since the focus is tools) that reads a file, searches the workspace, edits a file, and runs a shell command, asserting real filesystem side effects in a temp directory and the correct `AgentEvent` sequence (`ToolCallRequested` → `ToolCallStarted` → `ToolCallProgress`* → `ToolCallCompleted`).
- **Acceptance criteria:** test passes deterministically; temp directory is cleaned up after the test.
- **Effort:** M
- **Depends on:** Task 4.8

---

## Testing (this phase)

- Real I/O tests against temp directories (filesystem tools) and real child processes (shell tool), all deterministic and self-cleaning.
- Permission-policy branch coverage (Allow/Ask/Deny) per Task 4.7.
- Cancellation-of-tool-in-flight coverage (shell process kill) per Task 4.6.

## Exit criteria

- All four tools (`fs.read`, `workspace.search`, `fs.edit`, `shell.exec`) work against a real workspace/process, gated correctly by capability + permission policy (spec Section 71 Phase 4 goal).
- An agent granted only a subset of tools never has visibility into ungranted tools, even if those tools are registered runtime-wide.
- Cancelling a running `shell.exec` reliably terminates the underlying process.

## Trade-offs / open decisions

- **`fs.edit` input shape:** whole-file replacement (simpler, chosen for this phase) vs. patch/diff format (more token-efficient, matches how most coding agents actually edit) — flagged as a fast-follow, not blocking Phase 4 exit.
- **`harness-tool-git`:** spec Section 65 lists a `tools/git` crate; not required for Phase 4's exit criteria (only the four named tools are). Scaffold remains empty until a concrete need (e.g. `git.push` referenced as a `deny` example in spec Section 19) arises — recommend implementing it opportunistically alongside Phase 6 or later once subagent workflows create real demand.

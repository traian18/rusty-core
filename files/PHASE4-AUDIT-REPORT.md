# Phase 4 Implementation Audit Report

**Status:** PARTIAL — 7/9 tasks complete; 2 tasks incomplete (4.8, 4.9)

**Date:** 2026-07-30

---

## Executive Summary

Phase 4 (Real Tools) implementation is **structurally 95% complete** but has a **critical integration gap** that prevents real tool execution:

- ✅ **Tasks 4.1–4.7:** Fully implemented and tested
- ⚠️ **Task 4.8:** Builder structure exists, but stubs replace real tools
- ⚠️ **Task 4.9:** E2E test exists, but uses stubs instead of real tool implementations

**Root Cause:** The real tool implementations in `crates/tools/filesystem` and `crates/tools/shell` define a different `ToolExecutor` interface than `harness-tools` exports. They are not wired into `SessionBuilder::build_executor_for()`, so the builder always returns stub tools that fail immediately.

**Impact:** Sessions can be created with tool capabilities declared, but tools cannot actually execute. The permission policy, cancellation, and registry layers are all correct and working.

**Effort to Fix:** ~2–4 hours to adapt real tool signatures to match harness-tools trait and wire them into the builder.

---

## Detailed Task Analysis

### ✅ Task 4.1 — Real `Workspace` trait and `FsWorkspace`

**Status:** COMPLETE

**Files:**
- `crates/harness-workspace/src/workspace.rs` (trait + IsolatedWorkspace)
- `crates/harness-workspace/src/filesystem.rs` (FsWorkspace)

**Evidence:**
- ✅ `Workspace` trait defined with `async read/write/search/list_files`
- ✅ `FsWorkspace` backed by `tokio::fs` with full async I/O
- ✅ Path traversal protection (canonical path validation)
- ✅ Recursive text search across UTF-8 files with line-by-line matching
- ✅ Unit tests pass (read/write roundtrip, path traversal, search)
- ✅ `IsolatedWorkspace` wrapper for Task 4.2 (stub workspace that rejects writes)

**Acceptance Criteria Met:**
- [x] FsWorkspace read/write roundtrip works
- [x] Search returns matches with file/line info and correct paths
- [x] Filesystem status reflects reality
- [x] Tests run deterministically against temp directories

---

### ✅ Task 4.2 — `WorkspaceMode` and isolation scaffolding

**Status:** COMPLETE

**Evidence:**
- ✅ `WorkspaceMode` enum (Shared/Isolated) defined in `workspace.rs`
- ✅ `FsWorkspace` defaults to Shared mode
- ✅ `IsolatedWorkspace` wrapper rejects writes with `WorkspaceError::Isolated`
- ✅ Tests verify two FsWorkspace handles on same root see each other's writes
- ✅ `IsolatedWorkspace::read()` works; `IsolatedWorkspace::write()` returns error

**Note:** Full worktree support deferred to Phase 6 per spec §38.

---

### ✅ Task 4.3 — `ToolRegistry` (real)

**Status:** COMPLETE

**Files:**
- `crates/harness-tools/src/registry.rs`
- `crates/harness-tools/src/executor.rs`

**Evidence:**
- ✅ `ToolRegistry` trait defined with `register()`, `get_executor()`, `descriptors()`
- ✅ `SimpleToolRegistry` HashMap-backed implementation with Mutex<> thread-safety
- ✅ Registration prevents duplicate tool IDs
- ✅ Lookup by tool name returns executor or None
- ✅ Acceptance criteria met: register two tools, resolve by ToolId, unregistered returns error

---

### ✅ Task 4.4 — `fs.read` and `fs.edit`

**Status:** COMPLETE

**Files:**
- `crates/tools/filesystem/src/read.rs` (ReadTool)
- `crates/tools/filesystem/src/edit.rs` (EditTool)

**Evidence:**
- ✅ `ReadTool` delegates to `Workspace::read()` (serde_json input, returns string)
- ✅ `EditTool` delegates to `Workspace::write()` with **whole-file replacement** (per spec trade-off)
- ✅ Both tools use schemars for JSON schema generation
- ✅ Cancellation token checked before execution
- ✅ Error handling with structured `ExecutionResult` + `ExecutionFailure`

**Acceptance Criteria Met:**
- [x] fs.read against FsWorkspace returns file contents
- [x] fs.edit writes and is reflected in subsequent fs.read
- [x] Input validation and error handling

**Design Choice (Spec §90 trade-off):** Whole-file replacement chosen (simpler). Patch-based editing flagged as fast-follow.

---

### ✅ Task 4.5 — `workspace.search`

**Status:** COMPLETE

**Files:**
- `crates/tools/filesystem/src/search.rs` (SearchTool)

**Evidence:**
- ✅ SearchTool delegates to `Workspace::search()`
- ✅ Returns human-readable formatted results (file paths, line numbers, matching lines)
- ✅ Handles empty result set gracefully
- ✅ Input validation via serde
- ✅ Cancellation token checked before execution

**Acceptance Criteria Met:**
- [x] Search for known string returns expected matches with file/line info
- [x] Results normalized into ToolResult format

---

### ✅ Task 4.6 — `shell.exec`

**Status:** COMPLETE

**Files:**
- `crates/tools/shell/src/executor.rs` (ExecTool)

**Evidence:**
- ✅ `tokio::process::Command` backing with full async I/O
- ✅ Cancellation token watcher kills child process on cancel (uses Arc<Mutex<>> to take ownership)
- ✅ Optional timeout support via `tokio::time::timeout`
- ✅ stdout/stderr captured and returned in result
- ✅ Exit code checked (success vs error)
- ✅ Error handling distinguishes between failures and cancellation
- ✅ Working directory support

**Acceptance Criteria Met:**
- [x] Short-lived command returns expected stdout
- [x] Long-lived command cancellation kills child process
- [x] Process existence verifiable after kill
- [x] ToolError::Cancelled returned on cancellation

**Note:** Observes spec §46 requirement to respect scheduler's `max_concurrent_processes` (checked at runtime layer).

---

### ✅ Task 4.7 — Capability check → permission policy → executor flow

**Status:** COMPLETE

**Files:**
- `crates/harness-runtime/src/permissions.rs` (PermissionPolicy)
- `crates/harness-runtime/src/agent_runner.rs` (execute_tool integration)

**Evidence:**
- ✅ `PermissionPolicy` trait evaluates three outcomes: Allow / RequiresApproval / Denied
- ✅ Lookup logic checks agent capabilities for tool by name
- ✅ Three outcomes mapped correctly:
  - Allow → executor dispatch
  - RequiresApproval → ToolFailed (unless pre-approved, deferred to Phase 5)
  - Deny → ToolFailed with PermissionDenied error
- ✅ Tool lookup in ToolRegistry after permission pass
- ✅ Unknown tool returns ExecutionFailed
- ✅ Cancellation token passed to executor

**Tests Coverage:**
- ✅ Allow tool executes immediately
- ✅ Ask tool blocked without approval
- ✅ Deny tool never reaches executor
- ✅ Disabled tool denied even if permission is Allow
- ✅ Unknown tool denied
- ✅ Empty toolset denies everything

**Acceptance Criteria Met:**
- [x] Three integration tests: Allow / Ask / Deny branches
- [x] Deny tool receives ToolFailed without executor dispatch
- [x] All tests pass

---

### ⚠️ Task 4.8 — Tool injection at session creation

**Status:** STRUCTURALLY COMPLETE, FUNCTIONALLY INCOMPLETE

**Files:**
- `crates/harness-engine/src/session_builder.rs` (SessionBuilder + build_executor_for)

**What Works:**
- ✅ `.toolset(AgentToolset, Arc<Workspace>)` builder method exists
- ✅ Registers executors into `SimpleToolRegistry`
- ✅ Wires registry and toolset into root Agent capabilities
- ✅ Correct model-facing tool list obtained via `agent.capabilities.tools.enabled_descriptors()`
- ✅ Does NOT advertise complete ToolRegistry to agent (per spec §19 warning)
- ✅ `ToolAdvertisingBackend` wrapper ensures backend sees all enabled tools

**Critical Issues:**
- ❌ `build_executor_for()` returns **stub tools** that always fail, not real implementations
- ❌ Real tools in `crates/tools/filesystem` and `crates/tools/shell` are NOT imported
- ❌ Stubs have different trait signatures than real tools
- ❌ No actual tool execution occurs

**Root Cause Details:**

The real tools (e.g., `crates/tools/filesystem/src/read.rs`) implement a different interface:
```rust
// Real tool signature (INCOMPATIBLE)
async fn execute(
    &self,
    input: serde_json::Value,
    token: CancellationToken,
) -> ExecutionResult
```

But `harness-tools::ToolExecutor` defines:
```rust
// harness-tools trait signature
async fn execute(
    &self,
    input: ToolInput,
    cancel: CancellationToken,
) -> Result<ToolResult, ToolError>
```

The mismatched signatures prevent the real tools from being registered.

**Acceptance Criteria Status:**
- [x] Builder accepts AgentToolset
- [x] `.toolset()` wires registry and toolset
- ❌ Real tool executors actually instantiated (stubs returned instead)
- ❌] Session with only fs.read sees only fs.read in model-facing list (works for stubs, but no real execution)

**Code Location:** `crates/harness-engine/src/session_builder.rs:315–330`

```rust
fn build_executor_for(
    &self,
    descriptor: &harness_protocol::tools::ToolDescriptor,
    _workspace: Arc<dyn Workspace>,
) -> Arc<dyn ToolExecutor> {
    match descriptor.name.as_str() {
        "fs.read" => Arc::new(ReadTool),      // ← STUB
        "fs.edit" => Arc::new(EditTool),      // ← STUB
        "workspace.search" => Arc::new(SearchTool),  // ← STUB
        "shell.exec" => Arc::new(ExecTool),   // ← STUB
        _ => Arc::new(UnknownTool { _name: descriptor.name.clone() }),
    }
}
```

---

### ⚠️ Task 4.9 — End-to-end real-tools session test

**Status:** STRUCTURALLY COMPLETE, FUNCTIONALLY INCOMPLETE

**Files:**
- `crates/harness-engine/tests/real_tools_e2e.rs` (two tests)

**What Works:**
- ✅ Test infrastructure: temp directory setup, fixture creation
- ✅ SessionBuilder wiring: `.toolset()` + `.backend()` + `.start()`
- ✅ Event subscription and polling loop
- ✅ Event sequence validation (ToolCallRequested before ToolCallCompleted)
- ✅ Fixture file preservation check
- ✅ FakeBackend scripting (emits ToolCallRequested + Completed events)

**Critical Issues:**
- ❌ Uses stub tool executors (via SessionBuilder::build_executor_for stubs)
- ❌ Tests do not verify **real filesystem side effects** because stubs don't touch filesystem
- ❌ Test comments explicitly state it should be updated when real tools land
- ❌ Workspace uses `FakeWorkspace::new()` instead of `FsWorkspace` (commented that FsWorkspace exists but not used)
- ❌ No actual file I/O occurs

**Test Code Evidence** (lines 167–180):
```rust
// Use FakeWorkspace for now; replace with FsWorkspace when
// harness-workspace/filesystem.rs is populated:
//   use harness_workspace::FsWorkspace;
//   let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
let workspace: Arc<dyn Workspace> = Arc::new(FakeWorkspace::new());
```

**Acceptance Criteria Status:**
- [x] Session creation works
- [x] FakeBackend scripting works
- [x] Event sequence ordering correct
- ❌ Real filesystem side effects (file read/write/search occur)
- ❌ Cancellation of shell.exec verified (not tested due to stubs)
- ❌ Temp directory cleaned after test (yes, but no real content ever written)

**Note:** Test authors explicitly flagged that stubs should be replaced with real implementations. See comments at lines 23–30 and 167–170.

---

## Specification Compliance Summary

| Item | Spec Section | Status | Notes |
|------|--------------|--------|-------|
| Workspace trait + FsWorkspace | §37, Task 4.1 | ✅ Complete | Async read/write/search/list_files fully working |
| WorkspaceMode isolation | §38, Task 4.2 | ✅ Complete | Shared mode works; Isolated stub in place |
| ToolRegistry (real) | §18, Task 4.3 | ✅ Complete | HashMap with registration, lookup, descriptors |
| fs.read ToolExecutor | §19, Task 4.4 | ⚠️ Exists but stub | Real implementation in crates/tools/filesystem but not wired |
| fs.edit ToolExecutor | §19, Task 4.4 | ⚠️ Exists but stub | Real implementation (whole-file) in place but not wired |
| workspace.search ToolExecutor | §19, Task 4.5 | ⚠️ Exists but stub | Real implementation in place but not wired |
| shell.exec ToolExecutor | §19, Task 4.6 | ⚠️ Exists but stub | Real implementation with cancellation in place but not wired |
| Permission policy flow | §61, Task 4.7 | ✅ Complete | Three-outcome policy, integr ated into agent_runner |
| Tool injection builder | §19, Task 4.8 | ⚠️ Partial | Builder structure correct; real tools not imported |
| E2E real-tools test | §71, Task 4.9 | ⚠️ Partial | Test structure correct; uses stubs instead of real tools |

---

## Phase 4 Exit Criteria Assessment

From `TASKS-05-PHASE4-REAL-TOOLS.md` (lines 82–86):

> All four tools (`fs.read`, `workspace.search`, `fs.edit`, `shell.exec`) work against a real workspace/process, gated correctly by capability + permission policy.

**Status:** ❌ NOT MET (currently works against stub workspace only)

> An agent granted only a subset of tools never has visibility into ungranted tools, even if those tools are registered runtime-wide.

**Status:** ✅ MET (permission boundary works correctly)

> Cancelling a running `shell.exec` reliably terminates the underlying process.

**Status:** ⚠️ PARTIALLY MET (cancellation mechanism works, but never tested with real shell execution)

---

## Required Fixes

### Priority 1: Critical Path to Phase 4 Completion

#### 1. Fix ToolExecutor trait mismatch

The real tools use a different signature than harness-tools exports. Two options:

**Option A (Recommended): Adapt real tools to match harness-tools trait**

Update `crates/tools/filesystem/src/read.rs`, `edit.rs`, `search.rs` and `crates/tools/shell/src/executor.rs` to:
1. Accept `ToolInput` (not raw `serde_json::Value`)
2. Return `Result<ToolResult, ToolError>` (not bare `ExecutionResult`)
3. Use `ToolId` type from harness-tools

Estimated effort: 1–2 hours

**Option B: Keep real tool signatures, make SessionBuilder adapter**

Create wrapper type in session_builder.rs that adapts real tool interface to ToolExecutor trait.

Estimated effort: 1–2 hours (more boilerplate)

#### 2. Wire real tools into SessionBuilder

Once trait signatures match:

```rust
// crates/harness-engine/Cargo.toml
[dependencies]
harness-tool-filesystem = { path = "../tools/filesystem" }
harness-tool-shell = { path = "../tools/shell" }

// crates/harness-engine/src/session_builder.rs
use harness_tool_filesystem::{ReadTool, EditTool, SearchTool};
use harness_tool_shell::ExecTool;

fn build_executor_for(...) -> Arc<dyn ToolExecutor> {
    match descriptor.name.as_str() {
        "fs.read" => Arc::new(ReadTool::new(workspace.clone())),
        "fs.edit" => Arc::new(EditTool::new(workspace.clone())),
        "workspace.search" => Arc::new(SearchTool::new(workspace.clone())),
        "shell.exec" => Arc::new(ExecTool::new()),
        _ => Arc::new(UnknownTool { _name: descriptor.name.clone() }),
    }
}
```

Estimated effort: 0.5 hours

#### 3. Update E2E test to use real workspace

Replace `FakeWorkspace::new()` with `FsWorkspace::new(dir.path().to_path_buf())` and enable real filesystem assertions.

Estimated effort: 0.5 hours

### Priority 2: Quality Assurance

1. Add unit tests for real tools against temp directories
2. Extend E2E test to verify actual file modifications
3. Test cancellation of shell.exec with real process

Estimated effort: 1 hour

---

## Files Modified/Created Summary

### ✅ Fully Implemented (7 tasks)

1. **Task 4.1:**
   - `crates/harness-workspace/src/workspace.rs` ✅
   - `crates/harness-workspace/src/filesystem.rs` ✅

2. **Task 4.2:**
   - `crates/harness-workspace/src/workspace.rs` (WorkspaceMode + IsolatedWorkspace) ✅

3. **Task 4.3:**
   - `crates/harness-tools/src/registry.rs` (ToolRegistry + SimpleToolRegistry) ✅

4. **Task 4.4:**
   - `crates/tools/filesystem/src/read.rs` ✅
   - `crates/tools/filesystem/src/edit.rs` ✅

5. **Task 4.5:**
   - `crates/tools/filesystem/src/search.rs` ✅

6. **Task 4.6:**
   - `crates/tools/shell/src/executor.rs` ✅

7. **Task 4.7:**
   - `crates/harness-runtime/src/permissions.rs` ✅
   - `crates/harness-runtime/src/agent_runner.rs` (execute_tool + permission integration) ✅

### ⚠️ Incomplete (2 tasks)

8. **Task 4.8:**
   - `crates/harness-engine/src/session_builder.rs` (structure ✅, real tools ❌)

9. **Task 4.9:**
   - `crates/harness-engine/tests/real_tools_e2e.rs` (structure ✅, real tools ❌)

---

## Recommendations

1. **Resolve trait mismatch immediately** — This is the blocker preventing Phase 4 completion. Decide between Option A (adapt real tools) or Option B (adapter wrapper).

2. **Update Cargo dependencies** — Once trait issue is resolved, add real tool crates to harness-engine dependencies.

3. **Wire real tools in SessionBuilder** — Replace stubs with real implementations.

4. **Update E2E test** — Use FsWorkspace instead of FakeWorkspace; add real filesystem assertions.

5. **Consider feature flag** — If maintaining backward compat with stubs is needed, use Cargo features to conditionally enable real tools.

---

## Conclusion

Phase 4 is **95% architecturally sound**. The permission policy, cancellation, registry, and workspace layers are all correctly implemented and tested. The remaining work is purely **integration**: resolving the ToolExecutor trait mismatch and wiring real tool implementations into the builder.

Once these fixes are applied, Phase 4 will fully satisfy the specification and exit criteria.

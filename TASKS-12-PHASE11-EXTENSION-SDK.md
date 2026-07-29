# Phase 11 — Extension SDK

**Goal (spec Section 71):** stabilize tool registration, integration factories, context providers, event observers, and interceptors as a public extension surface; then add subprocess plugins, an MCP adapter, and (optionally) WASM.
**Depends on:** Phase 10 complete.
**Crates touched:** `harness-extension-api`, `harness-context`, `harness-tools` (interceptor hooks), `harness-runtime` (observer/interceptor invocation points).

**Locked decision reminder:** avoid Rust `cdylib` plugins as the primary third-party extension model (spec Section 55). Layer order: linked Rust crates → subprocess/JSON-RPC plugins → MCP adapter (`rmcp`) → WASM later if needed.

---

## Tasks

### Task 11.1 — `ExtensionRegistry` consolidation
- **Files:** `crates/harness-extension-api/src/registry.rs`
- **Description:** Implement `ExtensionRegistry` (spec Section 53) as the single struct aggregating `ToolRegistry` (Phase 4), `IntegrationRegistry` (Phase 3/6), `ContextProviderRegistry` (new, Task 11.3), `CommandRegistry` (new), `ObserverRegistry` (Task 11.4), `InterceptorRegistry` (Task 11.4). This is largely a consolidation/re-export task if the earlier phases' registries are already well-factored; use it to catch and fix any accidental coupling that crept in.
- **Acceptance criteria:** `harness-engine`'s `Harness::builder()` accepts registrations for every extension point through one consistent pattern (`register_tool`, `register_integration`, `register_context_provider`, `register_observer`, `register_interceptor`, `register_extension` for bundles).
- **Effort:** M
- **Depends on:** Phase 10 complete

### Task 11.2 — `ContextProvider` trait and pipeline
- **Files:** `crates/harness-context/src/provider.rs`, `crates/harness-context/src/engine.rs`
- **Description:** Implement `#[async_trait] pub trait ContextProvider` (spec Section 56), the context pipeline (Section 57: system instructions + conversation + project rules + workspace context + IDE context + tool definitions + extension context → `ExecutionRequest`), and at least two concrete providers to prove the pattern (`ProjectRulesContext` reading a rules file via the injected `Workspace`; `GitContext` reading current branch/status via a git command or library). The agent must never need to know where context came from (spec Section 56's closing line) — verify `harness-core` has zero references to any concrete `ContextProvider`.
- **Acceptance criteria:** a session configured with both providers produces an `ExecutionRequest` whose assembled context includes both providers' output in the pipeline order documented in Section 57.
- **Effort:** L
- **Depends on:** Task 11.1

### Task 11.3 — Context budget and compaction
- **Files:** `crates/harness-context/src/budget.rs`, `crates/harness-context/src/compaction.rs`
- **Description:** Implement `ContextBudget` (spec Section 58: `max_tokens`, `reserved_output_tokens`, `compaction_threshold`) and the compaction check performed between backend/tool iterations (not just between user turns, per the spec's explicit requirement). Compaction strategy for this phase: a straightforward history-summarization/truncation transform (`HistoryCompaction`, spec Section 57) sufficient to keep a long-running agent within budget; more sophisticated strategies are a future enhancement, not required for exit criteria.
- **Acceptance criteria:** a test drives a long scripted tool-iteration loop against a small `max_tokens` budget and confirms compaction triggers mid-run (not only at user-turn boundaries) and the resulting context stays within budget.
- **Effort:** L
- **Depends on:** Task 11.2

### Task 11.4 — Observers and interceptors
- **Files:** `crates/harness-extension-api/src/observers.rs`, `crates/harness-extension-api/src/interceptors.rs`, `crates/harness-runtime/src/agent_runner.rs` (invocation points)
- **Description:** Implement the observer/interceptor distinction from spec Section 54: observers (`on_tool_finished(&ToolResult)`, and equivalents for backend requests, agent completion) are read-only and cannot alter execution; interceptors (`before_tool(ToolRequest) -> Result<ToolRequest, InterceptorError>`, and an equivalent `before_backend_request`) may alter, deny, or enrich a request. Wire invocation points into `AgentRunner`'s effect interpreter at the appropriate points (before dispatching `ExecuteTool`/`ExecuteBackend`, after their results arrive).
- **Acceptance criteria:** a test observer counts tool completions without affecting results; a test interceptor denies a specific tool call by returning an error, and the denial surfaces as a normal `ToolFailed` outcome (not a crash); a second interceptor rewrites a tool request's arguments and the rewritten arguments are what the executor actually receives.
- **Effort:** L
- **Depends on:** Task 11.1

### Task 11.5 — Subprocess/JSON-RPC tool plugins
- **Files:** `crates/harness-extension-api/src/subprocess_plugin.rs`
- **Description:** Implement a `ToolExecutor` adapter that proxies to an external subprocess speaking JSON-RPC over stdio (using `jsonrpsee`, the locked decision from `TASKS-00-OVERVIEW.md` §2), allowing third-party tools to be added without any Rust code linked into the harness process. Define a small, stable plugin protocol (tool descriptor exchange on startup, execute request/response, progress notifications, cancellation notification).
- **Acceptance criteria:** a minimal example plugin (a trivial external process implementing the protocol) is registered as a tool and successfully executed through the normal capability/permission/executor flow from Phase 4, indistinguishable at the `AgentRunner` level from an in-process `ToolExecutor`.
- **Effort:** L
- **Depends on:** Task 11.1

### Task 11.6 — MCP adapter
- **Files:** `crates/harness-extension-api/src/mcp.rs` (or a dedicated new crate `harness-mcp-adapter` if the surface grows large — evaluate at implementation time)
- **Description:** Using `rmcp` (official `modelcontextprotocol/rust-sdk` crate, per locked research decision), implement an adapter exposing MCP servers' tools as `ToolExecutor`s (harness acting as an MCP client) and, optionally, exposing the harness's own tools as an MCP server (for interop with other MCP-aware hosts). Verify `rmcp`'s current crates.io version/maturity before pinning — this crate was noted as pre-1.0 and rapidly iterating as of the research pass in this planning cycle.
- **Acceptance criteria:** connecting to a reference/example MCP server and successfully listing + executing at least one of its tools through the normal harness tool-capability flow.
- **Effort:** L
- **Depends on:** Task 11.5

### Task 11.7 — Extension SDK stabilization pass
- **Files:** `crates/harness-extension-api/src/lib.rs` (public API docs), possibly a `CHANGELOG.md` addition
- **Description:** Review every public trait/type introduced across Phases 1–11 that a third party would need (`ExecutionBackend`, `ToolExecutor`, `Workspace`, `ContextProvider`, `IntegrationFactory`, observer/interceptor traits, plugin protocol types) for naming consistency, doc-comment completeness, and whether `cargo-semver-checks` (introduced in Phase 0's CI, Task 0.10) is actually gating this surface. Tag/document this as the first "stable-ish" extension SDK milestone.
- **Acceptance criteria:** every public trait listed above has a complete rustdoc comment with at least one usage example; `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS="-D warnings"` passes cleanly.
- **Effort:** M
- **Depends on:** Tasks 11.1–11.6

### Task 11.8 — WASM plugins (explicitly deferred)
- **Files:** none
- **Description:** Per spec Section 55, WASM plugin support is explicitly "later if needed" — do not implement in this phase. Record this decision so it isn't silently forgotten: WASM becomes worth revisiting only if subprocess/JSON-RPC plugins (Task 11.5) prove insufficient for a concrete third-party use case (e.g. sandboxing requirements stronger than a subprocess boundary provides).
- **Acceptance criteria:** N/A (explicitly not built); this task exists only to make the deferral visible and intentional.
- **Effort:** —
- **Depends on:** N/A

---

## Testing (this phase)

- Context pipeline and compaction-trigger tests (Tasks 11.2, 11.3).
- Observer/interceptor behavior tests, including the deny-and-rewrite cases (Task 11.4).
- End-to-end subprocess plugin and MCP adapter tests using minimal reference implementations (Tasks 11.5, 11.6).
- Full-workspace doc-lint pass (Task 11.7).

## Exit criteria

- Every extension point named in spec Section 52 (tools, execution backends, model clients, context providers, commands, event observers, lifecycle interceptors, workspace providers, session metadata, policy providers) has a concrete, documented registration path through `ExtensionRegistry`.
- Third-party tools can be added via subprocess/JSON-RPC or MCP without any change to `harness-core`, `harness-runtime`, or `harness-engine` source — the defining test of this entire project's architecture (spec Section 1's closing design rule, and Section 77's final architectural principle).
- WASM support is explicitly and visibly deferred, not silently missing.

## Trade-offs / open decisions

- **`rmcp` version/maturity:** flagged repeatedly because it was the least certain research finding (pre-1.0, fast-moving); budget extra time in Task 11.6 for API churn versus what's assumed here.
- **MCP client vs. server role:** implementing the harness as an MCP *client* (consuming external MCP tool servers) is the higher-value, lower-risk direction and should be built first; harness-as-MCP-*server* is valuable but optional for this phase's minimum exit criteria.
- **Whether `harness-context`'s compaction strategy is "good enough":** the simple summarization/truncation approach in Task 11.3 is a deliberate placeholder; treat context-quality tuning as an ongoing concern beyond this initial SDK milestone, not a one-time deliverable.

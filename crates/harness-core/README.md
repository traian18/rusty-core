# harness-core

The deterministic heart of the harness. An `Agent` is plain state with one `apply()` transition function: the same inputs always produce the same transitions and effects, regardless of timing or transport. The async runtime only *executes* what this crate decides, which is what makes sessions testable, replayable and identical across hosts.

## What's inside

- `agent`, `agent_state`, `transitions` — agent state, commands in, effects out.
- `behavior` — behavior profiles: what an agent is told, which tools it may use, how its loop is bounded, and the completion gate that can send an answer back for revision.
  - `definition.rs` owns profile documents, execution settings, and rule predicates; `definition/identity.rs`, `actions.rs`, and `completion.rs` group revision references, rule effects, and completion evaluators respectively.
  - `compiler`, `default`, and `registry` handle profile installation; `rules`, `state`, and `children` handle evaluation, persistence, and child resolution.
- `orchestration` — workflow definitions, the compiler that validates them, and the pure run-state reducer (steps, edges, retries, task plans). Runtime adapters execute the effects it emits.
  - `definition.rs` owns workflow documents and node configuration; `definition/identity.rs`, `bindings.rs`, `policies.rs`, and `task_queue.rs` group identifiers, graph data flow, execution budgets, and queue planning/review.
  - `compiler`, `default`, and `registry` handle definition installation; `state` owns run transitions and `task_plan` owns the shared plan schema.

Both domains keep their definition submodules private and re-export the same public types through `harness_core::behavior` and `harness_core::orchestration`. Consumers do not need implementation-specific import paths.
- `execution_policy`, `tool_alias`, `budget`, `capabilities` — application skill grants intersected with harness-owned mode limits, tool aliasing, budgets and per-agent capabilities.
- `context_state` — per-agent bookkeeping for the prepared inference context (pinned items, checkpoint lineage).
- `transcript`, `usage`, `content_hash` — transcript projection and usage accumulation.

**No I/O, by rule.** `harness-core` may depend only on `harness-protocol`; `cargo run --manifest-path xtask/Cargo.toml -- check-deps` fails the build if an HTTP, database, UI, transport or filesystem-walking dependency leaks in.

## In the workspace

- **Depends on:** `harness-protocol`
- **Used by:** `harness-engine`, `harness-runtime`

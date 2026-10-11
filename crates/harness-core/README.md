# harness-core

The deterministic heart of the harness. An `Agent` is plain state with one `apply()` transition function: the same inputs always produce the same transitions and effects, regardless of timing or transport. The async runtime only *executes* what this crate decides, which is what makes sessions testable, replayable and identical across hosts.

## What's inside

- `agent`, `agent_state`, `transitions` — agent state, commands in, effects out.
- `behavior` — behavior profiles: what an agent is told, which tools it may use, how its loop is bounded, and the completion gate that can send an answer back for revision.
- `orchestration` — workflow definitions, the compiler that validates them, and the pure run-state reducer (steps, edges, retries, task plans). Runtime adapters execute the effects it emits.
- `execution_policy`, `tool_alias`, `budget`, `capabilities` — application skill grants intersected with harness-owned mode limits, tool aliasing, budgets and per-agent capabilities.
- `context_state` — per-agent bookkeeping for the prepared inference context (pinned items, checkpoint lineage).
- `transcript`, `usage`, `content_hash` — transcript projection and usage accumulation.

**No I/O, by rule.** `harness-core` may depend only on `harness-protocol`; `cargo run --manifest-path xtask/Cargo.toml -- check-deps` fails the build if an HTTP, database, UI, transport or filesystem-walking dependency leaks in.

## In the workspace

- **Depends on:** `harness-protocol`
- **Used by:** `harness-engine`, `harness-runtime`

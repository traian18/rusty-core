# `harness-core` source modules

This directory contains the deterministic domain logic for agents and workflows. The crate is intentionally I/O-free: callers send commands and receive effects, while `harness-runtime` performs those effects.

## Agent and session-domain modules

| Module | Responsibility |
|---|---|
| [`agent.rs`](agent.rs) | Agent identity, immutable construction dependencies, and the usage ledger. |
| [`agent_state.rs`](agent_state.rs) | Mutable agent state, including lifecycle, transcript, pending work, children, and behavior state. |
| [`transitions.rs`](transitions.rs) | The pure command-to-effect reducer for agent lifecycle, tools, permissions, and completion. Backend-event handling is factored into [`transitions/backend_events.rs`](transitions/backend_events.rs). |
| [`capabilities.rs`](capabilities.rs) | Per-agent tool, workspace, backend, and child-spawning capabilities. |
| [`execution_policy.rs`](execution_policy.rs) | Intersects application grants with harness execution-mode restrictions and applies the resulting ceiling to tools. |
| [`tool_alias.rs`](tool_alias.rs) | Canonical tool aliases and the small set of tools admitted independently of execution policy grants. |
| [`budget.rs`](budget.rs) | Agent budget limits and accounting helpers. |
| [`context_state.rs`](context_state.rs) | Per-agent bookkeeping for prepared inference context, pinned items, and checkpoint lineage. |
| [`transcript.rs`](transcript.rs) | Transcript validation and projection helpers shared by agent transitions. |
| [`usage.rs`](usage.rs) | Usage summaries and aggregation for agents and child agents. |
| [`content_hash.rs`](content_hash.rs) | Stable content hashing used to identify behavior/profile content. |

## Behavior profiles

[`behavior/`](behavior/) defines, validates, installs, and evaluates behavior profiles. Its `definition` submodule owns the profile schema; `compiler` validates and compiles documents; `registry` resolves profile libraries and switch targets; `default` supplies the built-in profile; and `rules`, `state`, and `children` implement rule evaluation, persisted run state, and child-profile resolution. The `definition` submodules group identity, actions, and completion-gate definitions.

## Workflows and orchestration

[`orchestration/`](orchestration/) models workflow definitions and reduces workflow inputs into deterministic execution state. `compiler` validates graphs, `registry` installs definitions, `default` provides standard definitions, `state` owns run transitions, and `task_plan` defines shared task planning data. `definition` and its submodules group workflow schema, identifiers, bindings, policies, and task-queue data. Runtime adapters execute the effects produced here.

## Tests

Unit tests live beside their modules or in nested `tests.rs` files. Public behavior and cross-transition guarantees are covered by [`../tests/`](../tests/), particularly `transitions.rs`, behavior profile tests, and orchestration tests.

# `rusty-agent-core` source modules

The crate is I/O-free: callers send commands and receive effects, while `rusty-runtime` carries
the effects out. Read the crate README first; this page is the map of the files.

## Agent and session modules

| Module | Responsibility |
|---|---|
| `agent.rs` | Agent identity, construction dependencies, and the usage ledger |
| `agent_state.rs` | Mutable state: lifecycle, transcript, pending work, children, behavior state |
| `transitions/` | The reducer. `mod.rs` holds `Agent::apply`, the table of command to operation; one module per operation (see below) |
| `capabilities.rs` | Per-agent tool, workspace, backend and child-spawning capabilities |
| `execution_policy.rs` | Intersects application grants with execution-mode limits |
| `tool_alias.rs` | Tool names that grant others, and tools admitted regardless of grants |
| `budget.rs` | Agent budget limits and accounting |
| `context_state.rs` | Per-agent bookkeeping for the prepared inference context |
| `transcript.rs` | Transcript validation and projection |
| `usage.rs` | Usage summaries for agents and child agents |
| `content_hash.rs` | Stable content hashing for profile identity |

## `transitions/`

`run_start`, `backend_events`, `tool_request/` (admission checks, start, deny, rewrite),
`tool_result`, `completion/` (propose, evaluated verdicts, gate), `children`, `control`,
`behavior_profile`, `turn`, `lifecycle`, `ids`, `assistant_text`. Each file is the steps of one
decision.

## `behavior/`

`definition/` is the profile schema. `compiler/` validates a profile (`profile_checks`,
`rule_checks`, `condition_checks`, `gate_checks`) and answers questions about a compiled one
(`compiled`). `registry/` resolves profile libraries. `rules/` evaluates rules (`evaluate`,
`conditions`, `notes`, `final_status`). `state/` is the per-agent state (`turns`, `stored`).
`default.rs` is the built-in profile and `children.rs` resolves a child's profile.

Workflow definitions and their run state live in the sibling crate `rusty-workflow-core`.

# rusty-workflow-core

The deterministic half of workflows. A workflow is a graph of steps (input, agent, verify,
approval, subflow, output) joined by success and failure edges. This crate defines the document
that describes one, checks that it is sound, and holds the reducer that moves a run of it
forward: give it a command ("this step succeeded", "the user approved") and it changes the run's
state and answers with the effects to carry out ("start the next step", "ask the user").

Like `rusty-agent-core`, it does no I/O. Executing the steps is `rusty-workflow-runtime`'s job.

## Why it exists

A workflow run is long, branches, retries, pauses for people and sometimes gets restored after
a crash. The rules for that are easy to get subtly wrong: a retry that does not reset the right
downstream steps, a cancellation that races a completion, a restored run that picks up a step
whose side effects are unknown. Keeping those rules in pure functions over plain state means each
one can be tested with exact inputs and replayed from a recording. It is separate from the agent
core because a workflow is about steps and edges, while an agent is about a conversation and its
tools; `rusty-agent-core` is a dependency here (steps use its behavior profiles), never the other
way round.

## How it works

```text
  OrchestrationDefinition ──compile──► CompiledOrchestration
                                              │
  OrchestrationRunState ◄── apply(compiled, state, command) ──► Vec<OrchestrationEffect>
        ▲                                                              │
        └────────── the runtime executes the effects, reports outcomes as commands
```

1. **Compile.** `compile(definition)` runs the checks in order and returns every issue found, not
   just the first: the document itself (version, ids, budgets), each node, the edges, the graph
   shape (one input, at least one output, no cycles, everything reachable), what each node may
   read (only outputs of nodes that must have run before it), retry targets, and inline JSON
   Schemas. The result keeps lookups the reducer needs: nodes by id, edges by source and target,
   a topological rank, and the *retry span* of each node that can send work back.
2. **Run.** `OrchestrationRunState::new(run_id, &compiled)` creates a run. `apply` takes one
   `OrchestrationCommand` at a time and returns effects (`ExecuteStep`, `CancelStep`, `Emit` an
   event, `Finish` with the result). `ready_steps` says which steps may start now.
3. **Restore.** `check_restorable` and the `Recover` command rebuild a run after a restart: an
   interrupted agent or subflow step is failed as *indeterminate* because its side effects are
   unknown, while pure steps (input, verify, approval, output) are simply run again.

`apply` is a table, one function per command in `src/state/reducer/`: `start`, `finish`
(`succeed`, `fail`, `send_back`), `activate`, `permission`, `input_wait`, `control`
(pause, cancel, abort, recover) and `checkpoint`. `fail` reads as the ordered ways out of a
failed attempt: retry the same step, send work back to a retry target, ask for changes, take the
failure edge; with none of those the run fails.

## Key types and concepts

- **`OrchestrationDefinition`**: the document: nodes, edges, policies, an optional input schema
  and an output contract. `default_orchestration_definition()` is the built-in
  Input, Agent, Verify, Output flow.
- **Nodes**: `Input`, `Agent` (instructions, tool scope, profile, structured output, an optional
  task queue), `Verify` (schema, required status, artifacts, criteria and requirement checks, with
  a `retry_target`), `Approval` (a person reviews a subject; can send changes back), `Subflow`
  (run a saved flow or a one-step flow) and `Output`.
- **Bindings**: how a node gets its inputs: `RunInput { pointer }` or `NodeOutput { node_id,
  pointer }`, with JSON Pointers into the value.
- **`RetryPolicy` and `RetryReason`**: how many attempts a step gets and for which failures
  (`BackendRateLimited`, `BackendTimeout`, `ToolTimeout`, `InvalidStructuredOutput`,
  `VerificationFailed`, `ChangesRequested`).
- **`CompiledOrchestration`**: the validated definition plus lookups and a content hash that a
  restore uses to prove it resumes against the same document.
- **`OrchestrationRunState`**: the run's status (`Created`, `Ready`, `Running`,
  `WaitingForPermission`, `WaitingForInput`, `Paused`, `Cancelling`, `Completed`, `Failed`,
  `Cancelled`), every step's `StepRun` with its attempts, usage totals and the final output.
- **`DefinitionRegistry`**: definitions by id and revision, published or draft, with
  `LatestPublished` and exact references.
- **Task plans** (`task_plan`): the shared schema a planning step produces and a task queue
  consumes.

## A worked example

```rust
use rusty_workflow_core::{
    apply, compile, default_orchestration_definition, OrchestrationCommand,
    OrchestrationRunId, OrchestrationRunState,
};
use serde_json::json;

let compiled = compile(default_orchestration_definition())?;
let mut run = OrchestrationRunState::new(OrchestrationRunId::from("run-1"), &compiled);

let effects = apply(&compiled, &mut run, OrchestrationCommand::Start {
    input: json!({ "request": "add a --verbose flag" }),
})?;
// effects: RunStarted, StepReady("input"); the runtime now executes the input step and
// reports it with OrchestrationCommand::StepSucceeded { .. }, which readies "execute".

// Later, the verifier fails the report. "execute" allows two attempts and lists
// VerificationFailed in retry_on, so `fail` sends the work back:
//   StepFailed("verify"), StepRetryScheduled { node_id: "execute", next_attempt: 2, .. }
// and "execute" and "verify" are reset to Pending. A second failure ends the run with
// RunFailed, because no attempts or failure edge remain.
```

A definition with a mistake does not compile. For example, a verify node whose `retry_target` is
not upstream of it gives `invalid_retry_target` at `nodes.verify.config.retry_target`, along with
any other issue in the document.

## Public API

- `compile`, `CompiledOrchestration`, `DefinitionIssue`, `DefinitionValidationError`,
  `SUPPORTED_SCHEMA_KEYWORDS`.
- The definition types listed above and their ids (`OrchestrationDefinitionId`,
  `OrchestrationNodeId`, `OrchestrationEdgeId`, `OrchestrationRunId`).
- `apply`, `ready_steps`, `check_restorable`, `OrchestrationRunState`, `StepRun`,
  `OrchestrationCommand`, `OrchestrationEffect`, `OrchestrationEvent`, `OrchestrationResult`.
- `DefinitionRegistry`, `DefinitionRef`, `RegistryError`.
- `default_orchestration_definition`, `is_task_plan_schema`, `task_plan_schema`.
- `DefinitionStatus` and `ToolScope`, re-exported from `rusty-agent-core`.

## Errors

- `DefinitionValidationError` carries every `DefinitionIssue` (path, stable code, message), so an
  editor can show them together.
- `TransitionError` (code and message) is returned by `apply` for a command that does not fit the
  run: a terminal run, a stale attempt, a step that is not running. The runtime treats it as a
  bug in its own sequencing.
- `RegistryError` for unknown, duplicate or unpublished definitions.

## Design decisions

- **Every issue at once.** Compilation never stops at the first problem; authors fix a document
  in one pass.
- **No cycles in version 1.** The graph is a DAG, which is what makes "only read upstream outputs"
  and retry spans well defined.
- **Indeterminate means stop.** After a crash, an agent step may have changed files in ways the
  run cannot know, so it is failed rather than re-run silently.
- **Asking for changes is not failing.** A reviewer's request for changes goes back to the revise
  target without using up the target's failure retries, up to a revision limit.
- **Known exceptions to the size rules.** `apply` is a dispatcher over every command and stays
  one table (cyclomatic complexity 18); the built-in definition's `expect` in
  `DefinitionRegistry::with_builtin` states an invariant a test checks.

## Testing

`src/tests.rs` covers compilation errors, the reducer's transitions and restore, and
`tests/compatibility.rs` pins the public shapes and round trips. The runtime crate's tests run
real workflows through this reducer. Run `cargo test -p rusty-workflow-core`.

## In the workspace

- **Depends on:** `rusty-agent-core`, `rusty-protocol`
- **Used by:** `rusty-engine`, `rusty-runtime`, `rusty-workflow-runtime`

## Must never

- Do I/O, read the clock, or spawn tasks.
- Know how a step is executed, which model it uses, or what a tool does.
- Depend on anything but `rusty-agent-core`, `rusty-protocol` and plain data crates.

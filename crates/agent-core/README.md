# rusty-agent-core

The deterministic heart of the system. An `Agent` is plain state with one function, `apply`: give
it a command and it changes its state and answers with the effects the outside world should carry
out. It does no I/O, reads no clock and spawns nothing, so the same commands always produce the
same state and effects, in a test, in a replay, or in production.

## Why it exists

An agent run is a long conversation between a model, tools, a user and sometimes child agents.
Most of what goes wrong in such a system is a decision made in the wrong order: a permission asked
twice, a tool result lost when a run is cancelled, a completion accepted before its checks ran.
Those decisions are hard to test when they are tangled with network calls and timers. Keeping them
in a crate that cannot do I/O makes every one of them an ordinary function call that a test can
make with exact inputs, and makes sessions replayable: feed the recorded commands in again and the
same state comes out.

It is separate from the runtime on purpose. The runtime owns tasks, sockets and time; this crate
owns decisions. `cargo run --manifest-path xtask/Cargo.toml -- check-deps` fails the build if an
HTTP, database, UI or filesystem dependency ever reaches it.

## How it works

```text
  AgentCommand ──► Agent::apply ──► Vec<AgentEffect>
        ▲                                  │
        └──── the runtime executes each effect and reports the outcome as the next command
```

`Agent::apply` is a table: each kind of command goes to one operation, and each operation is a
module under `src/transitions/` that reads as the steps of that decision.

| Command | Operation | What it decides |
|---|---|---|
| `StartRun`, `Steer`, `FollowUp` | `run_start` | Start a run now, or queue the input behind the current one |
| `BackendEvent` | `backend_events` | Fold the model's streamed text, tool calls, usage and completion into the transcript |
| tool call requested | `tool_request` | Run it, ask the user first, or refuse it |
| `ToolCompleted`, `ToolFailed` | `tool_result` | Record the result, let rules react, then continue the turn |
| `PermissionResolved` | `tool_request` | Approved: run it. Denied: fail the call |
| end of a turn without tool calls | `completion` | Finish, or run the completion gate |
| `CompletionEvaluated` | `completion` | Judge the evaluators' verdicts and accept, send back, or give up |
| `ChildSpawned`, `ChildCompleted`, `ChildFailed` | `children` | Track child agents and their usage |
| `Cancel`, `Pause`, `Resume` | `control` | Stop, hold or continue a run |
| `SetBehaviorProfile` | `behavior_profile` | Install a profile and its switch library |
| `ConfigureExecution` | `turn` | Change model settings for the next request |

Take `tool_request` as the model for how the code is shaped. `Agent::tool_requested` is a short
list of steps: record the request, then `admit_tool_call` runs the checks in order (session
permission, profile scope, rules, combined permission, argument rewrite, tool budget). Each check
either lets the call through or returns the effects that refuse it, so no refusal has to be undone.
An admitted call then either runs now or waits for approval.

## Key types and concepts

- **`Agent`** and **`AgentState`**: identity, backend binding, capabilities, budget and usage
  ledger, plus the mutable state: status, transcript, pending tool calls and permissions,
  children, queued inputs and the behavior state.
- **Behavior profile** (`behavior`): a document saying what the agent is told (instructions per
  model family), which tools it may see (`ToolScope`), per-tool permission overrides, execution
  settings, limits (turns, tool calls, a final tool-less turn), declarative **rules**, and an
  optional **completion gate**. `compile` validates a profile and reports every issue at once;
  `ProfileRegistry` resolves profiles by id and revision; `BehaviorState` holds the run's counters.
- **Rules** (`behavior::rules`): `when` a condition holds on an event (before a tool call, after
  a result, before a model request, when a profile is entered), `do` an action: inject text,
  deny, ask, allow, rewrite arguments or result, stop the run, or switch profile. Evaluation is
  pure; the caller applies the outcome.
- **Completion gate**: checks that must pass before an answer is accepted. A check is a
  declarative condition, or an evaluator the runtime runs (a tool, a model, a sub-agent, a shell
  command). A failing answer is sent back with feedback, up to a number of continuations.
- **Execution policy, tool aliases, budget**: application grants intersected with harness mode
  limits, tool names that grant others (`write_file` covers `edit_file`), and per-agent limits.

## A worked example

A user starts a run, and the model asks for a tool the profile makes ask-first:

```rust
let mut agent = Agent::new(/* ids, backend, capabilities, budget */);

let effects = agent.apply(AgentCommand::StartRun { input });
// effects: StateChanged, ExecuteBackend { request }

let effects = agent.apply(AgentCommand::BackendEvent {
    run_id,
    event: ExecutionEvent::ToolCallRequested { call, .. },
});
// effects: ToolCallRequested event, StateChanged(WaitingForPermission),
//          PermissionRequested event, RequestPermission { request }

let effects = agent.apply(AgentCommand::PermissionResolved {
    id: permission_id,
    decision: PermissionDecision::Approved,
});
// effects: StateChanged(Executing), ExecuteTool { request }
```

Each call returns immediately with plain data. If the profile had denied the tool, the second call
would instead return a `ToolCallDenied` event and the failed call's result, and the transcript
would still be valid for the model's next turn.

## Public API

- `agent::Agent`, `agent_state::AgentState`: the state machine and its state.
- `transitions::MAX_CONSECUTIVE_TOOL_TIMEOUTS`: the limit after which a run fails.
- `behavior`: `compile`, `CompiledProfile`, `BehaviorProfile` and its parts, `ProfileRegistry`,
  `BehaviorState`, rule outcome types, `default_profile`, `profile_json_schema`.
- `capabilities`, `execution_policy`, `tool_alias`, `budget`, `usage`, `transcript`,
  `context_state`, `content_hash`: supporting modules.

## Errors

Nothing here returns an error to a caller of `apply`: a failure is a state. A run that cannot
continue ends with a `Failed` event and `FinishRun` (`fail` in `lifecycle`), and a refused tool
call becomes a failed tool result the model can read. Profile validation is the exception:
`compile` returns `ProfileValidationError` with every issue and its path
(`rules[2].when`), and `BehaviorState::from_stored` returns `BehaviorRestoreError` when a stored
profile no longer compiles or no longer matches its content hash, so a tampered snapshot fails
closed instead of running under different rules.

## Design decisions

- **State is data, decisions are functions of it.** No trait objects, channels or clocks. The
  timestamp and ids an operation needs come from counters in the state (`ids`), which is what
  makes replay exact.
- **A refusal is a return value.** Each admission check returns the effects that refuse the call,
  so the order of checks is the order of the code and nothing needs rolling back.
- **Events are listed exhaustively.** The projection that rebuilds state from recorded events
  names every event kind, so adding one forces a decision about what it changes.
- **Profiles fail closed.** A profile that does not validate, or whose hash differs from the
  recorded one, is not run.
- **Bounded by construction.** Assistant text, attachments and consecutive timeouts have limits
  here, so a hostile or runaway backend cannot grow an agent without bound.

## Testing

`tests/transitions.rs` drives whole runs through `apply` and asserts on the effects;
`tests/behavior_*.rs` cover rules, gates, rewrites and switching; `tests/determinism.rs` and the
`rc203_*` tests replay races and check that the same inputs give the same outputs;
`tests/compatibility.rs` pins the public shapes. Unit tests sit beside the behavior modules.
Run `cargo test -p rusty-agent-core`.

## In the workspace

- **Depends on:** `rusty-protocol`
- **Used by:** `rusty-engine`, `rusty-runtime`, `rusty-workflow-core`, `rusty-workflow-runtime`

## Must never

- Do I/O, read the clock, spawn tasks, or depend on anything but `rusty-protocol` and plain data
  crates.
- Know which provider, tool implementation or transport is in use.
- Return a different result for the same state and command.

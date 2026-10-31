# rusty-goals

Goal loops: an instruction and a result that counts as done, worked on in passes (each a fresh
agent run) until the result is reached, the budget runs out, or the agent is blocked. A quality goal
(targets on code health) is measured; a task goal (any other instruction) is worked through a plan kept
across passes, its checks and the pass's strict judgement.

## Why it exists

An agent that says "done" is not evidence. A goal gives the loop an outside judge: the host measures
the scope, this crate decides what the measurement means, and a pass that makes things worse is undone.
It has no I/O so the whole decision logic is testable.

## How it works

1. `contract` is the goal as saved: instruction, scope, targets, budget, status, plan and journal.
2. The host measures the scope; `measurement` holds what it found and which targets it misses.
3. `progress::compare` says whether a pass made things better, worse or changed nothing.
4. `decision::decide` turns a measurement and the pass's report into the goal's next state;
   `record` writes the pass in the journal.
5. `prompt` builds what the next pass is asked to do, in sections: header, done-when, scope, where it
   stands, earlier passes, the pass's task, and the strict judgement request.

## Key types and concepts

- **`Goal`**, **`Targets`**, **`Budget`**: what, how well, and for how long.
- **`Measurement`**: files, findings, guidelines and checks at one moment.
- **`Verdict`**: done, continue or undone for a pass.
- **Task goal vs quality goal**: a task ends on its plan and checks, a quality goal on measurements.

## A worked example

```rust
let before = host.measure(&goal);
let prompt = pass_prompt(&goal, &before);
// run the pass, measure again, then:
let decision = decide(&goal, &after, &report); // compare(&before, &after, &goal.targets) says better, worse or same
```

## Public API

- `Goal` and its parts, `Measurement`, `Progress`, `decide`, `record`, `compare`, `pass_prompt`,
  `status_text`.

## Errors

None: every function is total. An empty scope never meets a health target.

## Design decisions

- **The model never ends a goal by saying so.**
- **No I/O.** Measuring and running passes belong to the host.
- **Worse is undone**, so a loop cannot ratchet the code downward.

## Testing

`tests/goals.rs` covers decisions, progress and prompts. Run `cargo test -p rusty-goals`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** nothing else in the workspace

## Must never

- Do I/O or call a model.
- Treat a claim in the agent's report as the measurement.
- Keep a pass that made the measurement worse.

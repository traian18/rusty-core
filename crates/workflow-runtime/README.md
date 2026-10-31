# rusty-workflow-runtime

Runs workflows. `rusty-workflow-core` decides what runs next in an orchestration; this crate does
the I/O around it: it executes each step, gives every agent step its own isolated session, validates
structured boundaries, persists every transition, and runs the serial task queue that the build
steps of the spec workflows use.

## Why it exists

A workflow is a graph of steps (input, agent, verify, approval, subflow, output). The reducer in
the core is pure, so something has to start the sessions, wait for them, answer their permission
and question requests, enforce retries and budgets, and store every event so a stopped run can be
resumed. Keeping this apart from the session runtime means a workflow is just another client of
sessions: it uses `rusty-runtime` and never reaches into it.

## How it works

1. `OrchestrationRunner` compiles a definition, creates a run and returns an
   `OrchestrationHandle` for control (pause, resume, cancel, and answering a permission or an input
   request); `OrchestrationRunner::retry_failed` continues a failed run from its stored state. The run loop admits
   ready steps, executes them as `StepJob`s, and applies each outcome through the core reducer.
2. Agent steps go to an `AgentStepExecutor`. The normal one, `IsolatedSessionAgentExecutor`, opens
   a fresh session per attempt (`session`), tells it what to do (`prompt`), and follows it
   (`conversation`) until it answers, relaying permission requests to the run and applying the
   host's decisions. `research` lets a step ask a read-only research step a question.
3. A step's reply is read as the output schema's value: Markdown in the layout derived from the
   schema (`markdown_reply`), or JSON. Replies are validated by `BasicSchemaValidator`; an invalid
   reply is retried with the validation errors as feedback.
4. Every transition is stored (`OrchestrationStore`), so `retry_failed` and recovery continue from
   what is persisted. The `journal` tells each step what the others did, from their tool calls.
5. A step with a task queue runs `task_queue`: it reads the plan, resumes from the checkpoint,
   folds in rejections and the user's change requests, then works through the tasks. Each task is
   planned in detail (optional), built under a watchdog, typechecked by the workflow, and judged by
   a read-only reviewer. A task that cannot be completed goes to the queue's failure policy.

## Key types and concepts

- **`OrchestrationRunner` / `OrchestrationHandle`**: start and control a run.
- **`AgentStepExecutor`**: the contract an agent step runs through.
- **`IsolatedSessionAgentExecutor`**: runs each attempt in a fresh session and follows it.
- **`Conversation`**: the event loop of one attempt, one handler per wake-up.
- **Task queue**: `Plan` (requirements, criteria, tasks), `Progress` (what is checkpointed), and
  `Queue` (the working state of one run).
- **`Watchdog`**: stops a builder that keeps failing the same check or never edits.
- **`workspace_snapshot`**: checkpoints of the workspace's git repositories under hidden refs, and
  undoing back to one.

## A worked example

A planned task, in the order the queue treats it:

```text
plan the task in detail (or split it)  ->  build under the watchdog
  -> workflow typecheck: new errors go back to the builder
  -> read-only review: every criterion needs evidence
  -> accepted: checkpoint, next task
  -> not accepted: repair up to max_repairs times; if the build is kept the user decides,
     otherwise undo, split once, then ask the failure policy (stop, skip or ask)
```

## Public API

- `orchestration`: `OrchestrationRunner`, `OrchestrationHandle`, `OrchestrationUpdate`,
  `AgentStepExecutor`, `StepContext`, the schema types, and the stores
  (`InMemoryOrchestrationStore`, `FileOrchestrationStore`).
- `session_agent_executor::IsolatedSessionAgentExecutor` (with `with_profiles` and
  `with_subflows`) and `RESEARCH_TOOL`.
- `workspace_snapshot`: `take`, `undo_to`, `fingerprint`, `patch`.

## Errors

`OrchestrationRuntimeError` is what a run can fail with outside the workflow (storage, a lost
step). Step failures are `AgentExecutionError`s with a code; a retryable one carries a
`RetryReason` so the retry policy can apply. The task queue reports an unfinished task as a
failure outcome, never as a panic, and the queue's policy decides what happens next.

## Design decisions

- **The queue owns its progress.** A `Queue` run holds the progress and borrows everything else,
  and checkpoints after every change, so a stopped run resumes at the same task.
- **Rejections reopen, they do not restart.** A failed verification reopens the first task that
  owns an unmet criterion; the user's change requests become one repair task after the planned
  ones.
- **Reviewers never get a mutating tool**, even from a permissive profile.
- **A check that cannot run does not stop the work**; the reviewer judges the code that was written
  and the final validation reports what did not run.

## Testing

The orchestration suite drives whole runs with fake executors; the task queue has unit tests for
planning, feedback, checks, snapshots and splits. Run `cargo test -p rusty-workflow-runtime`.

## In the workspace

- **Depends on:** `rusty-agent-core`, `rusty-protocol`, `rusty-runtime`, `rusty-session-store`, `rusty-tool-api`, `rusty-workflow-core`, `rusty-workspace`
- **Used by:** `rusty-engine`

## Must never

- Decide the next step of a workflow: that is the reducer in `rusty-workflow-core`.
- Reach into a session's internals: it is a client of `rusty-runtime`.
- Let a reviewer or planner session change files.
- Treat a model's claim as evidence: actions come from the tool calls it made.

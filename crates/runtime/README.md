# rusty-runtime

The async half of the engine. `rusty-agent-core` decides what an agent should do next and
answers with effects; this crate carries those effects out on Tokio: it runs sessions and agents,
calls the model backend, executes tools, enforces permissions and cancellation, persists events,
and turns every outcome back into a command for the core. It also holds the contracts that
transports serve (`RpcHandler`, `RpcConnection`) and the host-facing traits other crates implement.

## Why it exists

The core is deliberately free of I/O so that its behavior can be tested exhaustively. Something
still has to own the sockets, tasks, timers, files and subprocesses, and that something must be
shared by every host (the IDE, the daemon, the CLI, the SDK). Putting it in one crate means a
session behaves the same wherever it runs, and the rule "a crash must not leave the store lying"
is enforced in one place: the session's committer is the only way events reach the store.

## How it works

1. A host builds a `SessionHost` (backend, tool registry, workspace, event sink, scheduler,
   integrations, optional store) and starts a `SessionRuntime` with `SessionRuntime::start`, or
   rebuilds one from a snapshot with `SessionRuntime::from_stored`. Both go through the same
   `assembly` step: cancellation tree, supervisor, event bus, live state, projection, committer,
   and one `AgentRunner` per agent.
2. An `AgentRunner` is the loop for one agent. It receives `AgentCommand`s, applies them to the
   core `Agent`, publishes the resulting events, and dispatches the effects the core returned:
   start a backend request (`backend_request`), run a tool (`tool_request`, after the agent's own
   permission rules allow it), spawn or cancel a child (`spawning`), persist a mutation
   (`outcome`), or ask the completion gate (`completion`). Each outcome comes back as a command.
3. The `Scheduler` bounds how many sessions, agents, backend requests, tool calls and processes
   run at once; backends can also have their own rate and concurrency limits.
4. The `AgentSupervisor` spawns child agents from a `SpawnHost` in eight named steps (gate, slot,
   backend, capabilities, workspace, cancellation, behavior, run) and never lets a child exceed
   its parent's budget, depth or tools.
5. The `completion_gate` runs the checks a behavior profile attaches to a final answer: a tool, a
   model question, a command, or a read-only verifier agent in an isolated session.
6. The `SessionManager` tracks active sessions, admits new ones through the scheduler, supervises
   their root tasks, and restores stored sessions (`session_manager::restorer`) after validating
   the trailing events, migrating the snapshot and re-creating every backend from the registered
   integrations.

## Key types and concepts

- **`SessionRuntime`**: one session. Owns the agents' state, the event bus and the committer.
- **`SessionHost`**: the host services a session runs on, shared by every agent in it.
- **`AgentRunner` / `AgentTask`**: the loop of one agent and its mailbox. `RunnerServices` groups
  the services a runner works with.
- **`AgentSupervisor` / `SpawnHost`**: child spawning with non-escalating limits.
- **`Scheduler`**: the permits that bound concurrency; `NeverClosed` documents why a closed
  semaphore cannot happen.
- **`SessionManager`**: the multi-session coordinator with the `SessionRegistry`,
  `SessionLifecycle` and `SessionRestorer` contracts.
- **`RpcConnection`**: the per-connection protocol state every transport shares.
- **`locks`**: `lock_or_recover` and friends, which keep working after a holder panicked.

## A worked example

```rust
let host = SessionHost {
    scheduler: scheduler.clone(),
    session_store: Some(store.clone()),
    ..SessionHost::new(session_id, backend, tool_registry, workspace, event_sink)
};
let runtime = SessionRuntime::start(host, root_toolset, None);
runtime.send_command(SessionCommand::Prompt(input)).await?;
let mut events = runtime.event_bus.subscribe();
```

Restoring is the same call with the stored agents: `SessionRuntime::from_stored(host,
stored_agents)?`, which fails with `RestoreError` when a stored behavior no longer validates or the
snapshot has no root agent.

## Public API

- Sessions: `SessionRuntime`, `SessionHost`, `SessionCommand`, `SessionSnapshot`, `SessionManager`.
- Agents: `AgentRunner`, `AgentTask`, `RunnerServices`, `AgentSupervisor`, `SpawnHost`,
  `SpawnOutcome`.
- Limits: `Scheduler`, `SchedulerConfig`, `resource_manager`, `cancellation`.
- Host contracts: `traits` (`ExecutionBackend`, `ToolRegistry`, `Workspace`, `EventSink`),
  `IntegrationRegistry`, `rpc` (`RpcHandler`, `RpcConnection`, `ResponseSink`).
- `testing` (behind the `testing` feature): fake backends, tools and workspaces.

## Errors

`SessionManagerError` covers admission, restore and backend creation. `SupervisorError` covers a
refused spawn (budget, capability, workspace, depth). `SessionError` is what a command to a running
session can fail with. Restoring never guesses: a missing dependency under a strict policy, an
invalid stored behavior, or a snapshot without a root is an error.

## Design decisions

- **Poison-tolerant locks.** The data behind the session locks is updated in whole steps, so a
  panic in one holder leaves it usable; later callers recover the guard instead of panicking too.
- **One assembly path.** A fresh and a restored session differ only in their agents, so they share
  `assembly`.
- **Parameter structs for hosts.** `SessionHost`, `SpawnHost` and `RunnerServices` replace long
  argument lists of same-shaped `Arc<dyn Trait>` values, which could be transposed silently.
- **Agent and session runtime are still one crate.** They call each other in cycles (a session
  builds the runner, the runner commits through the session). Separating them needs a trait
  boundary and is planned, not done.

## Testing

Unit tests sit beside each module; `tests/` holds the integration suites for concurrency, replay,
subagents and the spawn tool. Run `cargo test -p rusty-runtime`.

## In the workspace

- **Depends on:** `rusty-agent-core`, `rusty-protocol`, `rusty-session-store`, `rusty-tool-api`, `rusty-workflow-core`, `rusty-workspace`
- **Used by:** `harnessd`, `rusty-context`, `rusty-context-repo`, `rusty-engine`, `rusty-extension-api`, `rusty-harness-sdk`, `rusty-model-backend`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-gemini`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-provider-openai-compatible`, `rusty-provider-openai-responses`, `rusty-transport-ipc`, `rusty-transport-mcp`, `rusty-transport-stdio`, `rusty-transport-websocket`, `rusty-workflow-runtime`

## Must never

- Decide what an agent should do next: that is `rusty-agent-core`.
- Write to the store except through the session's committer.
- Depend on the engine, on providers, or on any concrete tool.
- Let a child agent exceed its parent's budget, depth or tool set.

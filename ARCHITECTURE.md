# Start here: how the code fits together

This page is for someone who did not write the code. It answers three questions: **where does each feature start**, **which file does the next step**, and **what to read first**. Every name below is a real function or type; search for it.

For what the project is and how to use it, read [README.md](README.md). For why each crate exists, read its own `README.md`.

## The idea in one paragraph

An *agent* is a small state machine. You give it a **command** ("start a run with this prompt", "the tool finished", "the model sent some text"). It changes its state and answers with a list of **effects** ("call the model", "run this tool", "publish this event", "save this"). A separate async layer carries out each effect and turns the outcome into the next command. The state machine does no I/O, so it behaves identically everywhere; everything that touches the world sits in the layer around it.

```
 command ──► Agent::apply ──► effects ──► AgentRunner carries them out ──┐
    ▲        (harness-core)               (harness-runtime)              │
    └─────────────────── outcome becomes the next command ◄──────────────┘
```

## Reading order

1. `crates/harness-protocol/src/commands.rs`, `effects.rs`, `events.rs`: the vocabulary (`AgentCommand`, `AgentEffect`, `AgentEvent`). Short, and everything else uses these words.
2. `crates/harness-core/src/transitions.rs`, starting at `Agent::apply`: the table of "command → what happens". Each arm calls one small method (`start_run`, `backend_event`, `tool_requested`, `tool_completed`, …).
3. `crates/harness-runtime/src/agent_runner.rs`, starting at `AgentRunner::run`: the loop that feeds commands to `Agent::apply` and executes the effects.
4. `crates/harness-engine/src/session_builder.rs`: how a host assembles all of it (`SessionBuilder::start`).

## Where each feature starts

### Create a session (embedding the engine in Rust)

| Step | Where |
|---|---|
| Build the engine once | `Harness::new()` / `Harness::builder()` in `crates/harness-engine/src/harness.rs`, `builder.rs` |
| Describe a session | `Harness::session()` returns a `SessionBuilder`; chain `.integration(...)`, `.workspace(...)`, `.tools(...)`, `.skills(...)`, `.profile(...)`, `.context_provider(...)` |
| Start it | `SessionBuilder::start()` in `session_builder.rs`: resolves the model backend, wraps it (tool advertising, context assembly), builds the tool registry, then asks the session manager to create the runtime |
| Register the session | `SessionManager::create_session` in `crates/harness-runtime/src/session_manager/manager.rs` |
| What you get back | a `SessionHandle` (`session_builder.rs`): `send`, `steer`, `follow_up`, `subscribe`, `cancel`, … |

### Send a prompt and run it

| Step | Where |
|---|---|
| Host calls | `SessionHandle::send` → `SessionHandle::send_input` |
| Forwarded | `SessionClient::send` (`crates/harness-runtime/src/session_client.rs`) |
| Turned into an agent command | `SessionRuntime::send_command` (`session_runtime/mod.rs`): `SessionCommand::Prompt` becomes `AgentCommand::StartRun` |
| The agent's loop picks it up | `AgentRunner::run` (`agent_runner.rs`): receive a command → `apply_and_publish` → `dispatch_effects` |
| The decision | `Agent::apply` → `start_run` (`harness-core/src/transitions.rs`) builds the model request and returns `AgentEffect::ExecuteBackend` |
| Calling the model | `AgentRunner::execute_backend` calls the `ExecutionBackend`; every streamed piece comes back as `AgentCommand::BackendEvent` |
| Model asked for a tool | `Agent::tool_requested` decides allow, ask or deny (profile rules, execution policy, permission mode) and returns `ExecuteTool` or `RequestPermission` |
| Running the tool | `AgentRunner::execute_tool`; the result comes back as `ToolCompleted` or `ToolFailed`, then `continue_after_tools` asks the model again |
| Finishing | `Agent::finish_success`; if the profile has a completion gate, `propose_completion` runs the checks first (`evaluate_completion` in the runner) |

### Where events come from

The agent's `Emit` effect calls `AgentRunner::emit`, which stamps the routing metadata and sequence numbers and publishes to the session's event bus (`session_runtime/event_bus.rs`). Durable events are also committed to the session store through the commit boundary in `session_runtime`; streaming deltas are never stored. A client reads them with `SessionHandle::subscribe`.

### A model backend

The agent only knows the `ExecutionBackend` trait. Almost every provider implements it by plugging a small `ModelClient` into `GenericModelBackend` (`crates/harness-generic-backend/src/backend.rs`, method `execute`), which adds retries, idle timeouts and the circuit breaker once for all providers. A provider crate (`crates/integrations/<name>/`) holds only that provider's wire format: `wire.rs` (request and response shapes), `client.rs` (the HTTP call), `config.rs` (settings), `usage.rs` (token accounting).

### Add a tool

Implement `ToolExecutor` (`crates/harness-tools/src/executor.rs`), register it in the session's tool registry, and it is available to the model like any built-in. Built-in tools are in `crates/tools/*`; the name-to-implementation table is `build_executor_for` in `crates/harness-engine/src/tool_factory.rs`. Sub-agent delegation (`agent_spawn`) is handled inside the runner (`AgentRunner::execute_spawn_tool`), not as a registered executor.

### Run it as a daemon

| Step | Where |
|---|---|
| Process starts | `main` in `apps/harnessd/src/main.rs`: registers the integrations, opens the session store, starts the chosen transports |
| A connection arrives | `serve` in the transport crate (`crates/transports/{ipc,websocket,stdio}`); frames are read per connection (`handle_connection`, or `serve_io` for stdio) and each request goes to `answer_one_request` |
| The request is understood | `HarnessRpcHandler::handle` in `apps/harnessd/src/handler.rs` maps each `RpcRequestBody` (create session, send, subscribe, …) onto the `Harness` |
| Anything else | the wire contract is `crates/harness-protocol/src/rpc.rs` |

### Run a workflow

`Harness::run_orchestration` (`crates/harness-engine/src/orchestration.rs`) hands the definition to the `OrchestrationRunner` (`crates/harness-runtime/src/orchestration/runner.rs`; `start` / `run`). The pure part (validating the graph, deciding what runs next) is `crates/harness-core/src/orchestration/` (`compiler.rs`, `state.rs`). Each agent step is executed by `run_single_agent_step` (`session_agent_executor.rs`) in an isolated session; a step with a task queue goes through `run_planned_task_queue` (`task_queue.rs`).

### Resume a stored session

`Harness::restore_session` → `SessionManager::restore_session` → the runtime is rebuilt from the latest snapshot plus the events after it (`session_runtime/restoration.rs`; the store contract is `crates/harness-session-store/src/store.rs`).

### Keep a long conversation inside the model's window

`crates/harness-context/`: `ContextAssemblingBackend` runs every request through a chain of `ContextProvider`s before it reaches the model. See that crate's README for the compaction tiers.

## Where to look when something is wrong

| Symptom | Start at |
|---|---|
| The agent did something unexpected after a model reply | `Agent::backend_event` and `Agent::tool_requested` in `transitions.rs` |
| A tool was denied or asked for permission | `Agent::tool_requested`, then `execution_policy.rs` and the behavior rules in `harness-core/src/behavior/` |
| An event is missing or out of order | `AgentRunner::emit` and `session_runtime/event_bus.rs` |
| A provider call fails or retries | `GenericModelBackend::execute` and the provider's `client.rs` |
| A session did not come back after a restart | `session_runtime/restoration.rs`, `harness-session-store/src/replay.rs` |
| A workflow stalls or retries | `harness-core/src/orchestration/state.rs` (the decisions) and `harness-runtime/src/orchestration/runner.rs` (the execution) |

## Rules that keep it understandable

- `harness-core` and `harness-protocol` never do I/O. `cargo run --manifest-path xtask/Cargo.toml -- check-deps` enforces it.
- Decisions live in `harness-core`; carrying them out lives in `harness-runtime`. If you are adding behavior, ask which half it is.
- Everything a client can observe is an event in `harness-protocol/src/events.rs`.

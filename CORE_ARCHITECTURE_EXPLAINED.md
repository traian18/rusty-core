# Core Architecture Explained

This document explains the core architecturally, including its major components, runtime data flow, extension points, persistence model, transports, and capacity controls.

## One-sentence explanation

**The core is an event-driven agent engine: a deterministic state machine decides what should happen, while an asynchronous runtime performs the real-world work—calling models, running tools, persisting state, and streaming events back to clients.**

The key separation is:

```text
Command → Core decision → Effects → Runtime execution → New command
```

For example:

```text
User prompt
   ↓
Agent::apply(...)
   ↓
ExecuteBackend effect
   ↓
Runtime calls the LLM
   ↓
Backend response becomes a new command
   ↓
Agent decides whether to answer, call a tool, request permission, or finish
```

The architectural principle is:

> **The core decides; the runtime executes.**

---

# Main components

## 1. Protocol — the vocabulary

**Crate:** `crates/harness-protocol`

This defines the messages that move through the system:

- `AgentCommand` — something that happened or was requested
- `AgentEffect` — work the runtime needs to perform
- `AgentEvent` — something observable by clients
- RPC messages and transport contracts
- Session, permission, admission, and error types

This layer is the shared language between the core, runtime, daemon, SDKs, and clients.

It does not perform I/O.

## 2. Core — the decision-making state machine

**Crate:** `crates/harness-core`

This is the pure decision layer.

It receives commands such as:

- Start a run
- Receive a model response
- A tool was requested
- A tool completed
- A permission decision arrived
- A run was cancelled

It updates the agent state and returns effects such as:

- Call the model
- Execute a tool
- Request permission
- Emit an event
- Persist state
- Finish the run

Conceptually:

```rust
// crates/harness-core/src/transitions.rs
let effects = agent.apply(command);

// The dispatch is explicit and side-effect free:
// StartRun       -> start_or_queue(input)
// BackendEvent   -> backend_event(run_id, event)
// ToolCompleted  -> tool_completed(call_id, result)
// PermissionResolved -> permission_resolved(id, decision)
// Cancel         -> cancel()
```

The public transition boundary is:

```rust
pub fn apply(&mut self, command: AgentCommand) -> Vec<AgentEffect> {
    match command {
        AgentCommand::StartRun { input } => self.start_or_queue(input),
        AgentCommand::BackendEvent { run_id, event } => self.backend_event(run_id, event),
        AgentCommand::ToolCompleted { call_id, result } => {
            self.tool_completed(call_id, result)
        }
        AgentCommand::Cancel => self.cancel(),
        // Other commands follow the same pattern.
        command => self.handle_other(command),
    }
}
```

The exact implementation is in [`crates/harness-core/src/transitions.rs`](crates/harness-core/src/transitions.rs). The core returns data describing work; it does not execute that work.

The core deliberately does not know how to:

- Make HTTP requests
- Execute shell commands
- Write to a database
- Manage Tokio tasks
- Open sockets

That makes the central behavior deterministic, testable, and independent of deployment environment.

The architecture document describes this as:

> An agent is a small state machine. It receives commands, changes its state, and produces effects. A separate asynchronous layer executes those effects.

## 3. Runtime — asynchronous execution

**Crate:** `crates/harness-runtime`

The runtime performs the effects created by the core.

It is responsible for:

- Running agent loops
- Calling model backends
- Executing tools
- Enforcing permissions
- Handling cancellation
- Managing concurrency
- Persisting events and snapshots
- Restoring sessions
- Running child agents
- Executing workflows and task queues

The runtime feeds results back into the core as new commands.

For example:

```text
Core returns ExecuteBackend
        ↓
Runtime calls provider
        ↓
Provider returns streamed response
        ↓
Runtime sends BackendEvent to core
        ↓
Core determines the next effects
```

### Concrete runtime package map

| Package/module | What to point to when explaining it |
|---|---|
| `harness-runtime::agent_runner` | Runs the command/effect loop and coordinates backend events. |
| `harness-runtime::agent_supervisor` | Starts and tracks child agents, acquiring scheduler permits. |
| `harness-runtime::scheduler` | Owns global/per-session semaphores and backend-specific limits. |
| `harness-runtime::session_manager` | Admits, creates, restores, and removes sessions. |
| `harness-runtime::session_runtime` | Owns the live session state and commit boundary. |
| `harness-runtime::cancellation` | Propagates cancellation to active work. |
| `harness-runtime::orchestration` | Coordinates workflows and multi-step execution. |
| `harness-runtime::task_queue` | Handles persisted queued tasks. |

Useful source files include [`agent_runner.rs`](crates/harness-runtime/src/agent_runner.rs), [`agent_supervisor.rs`](crates/harness-runtime/src/agent_supervisor.rs), and the module declarations in [`harness-runtime/src/lib.rs`](crates/harness-runtime/src/lib.rs).


## 4. Engine — application-facing composition

**Crate:** `crates/harness-engine`

This is the convenient public API for embedding the system in an application.

An application typically uses:

```rust
// crates/harness-engine/tests/integration_test.rs
let handle = Harness::new()
    .session()
    .backend(backend)
    .tools(tool_registry)
    .start()
    .await?;

let mut events = handle.subscribe();
handle.send("hello from test").await?;
```

This is a real embedding shape used by the integration tests: `Harness` creates a session builder, the builder receives an `ExecutionBackend` and tool registry, and `start()` returns a `SessionHandle`. The handle is the host application's control plane for sending input and subscribing to events. See [`crates/harness-engine/tests/integration_test.rs`](crates/harness-engine/tests/integration_test.rs).

A simplified lifecycle is:

```text
Harness::new()
   ↓
Harness::session()
   ↓
SessionBuilder configuration
   ↓
SessionBuilder::start()
   ↓
SessionHandle
```

The `SessionHandle` gives the host application operations such as:

- Send a prompt
- Steer an active run
- Send a follow-up
- Subscribe to events
- Cancel a run
- Restore or inspect a session

## 5. Model integrations — provider adapters

**Directories:** `crates/integrations/*`

The core does not depend directly on OpenAI, Anthropic, Gemini, or another model provider.

Instead, it depends on an abstraction:

```rust
// crates/harness-runtime/src/traits.rs
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;
    fn capabilities(&self) -> BackendCapabilities;

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ProtocolExecutionResult, ExecutionError>;
}
```

The backend must normalize provider output into `ExecutionEvent` values and observe cancellation. The production generic implementation is `GenericModelBackend` in [`crates/harness-generic-backend/src/backend.rs`](crates/harness-generic-backend/src/backend.rs); provider integrations sit below that abstraction. A minimal test backend can stream events through `sink` and return a final result, which is demonstrated by `ScriptedBackend` in [`crates/harness-engine/tests/integration_test.rs`](crates/harness-engine/tests/integration_test.rs).

Provider-specific integrations implement the provider wire format and HTTP behavior.

Typical provider responsibilities are separated into:

- Request and response wire types
- HTTP client
- Configuration
- Authentication
- Token and usage accounting

The generic backend adds shared behavior such as:

- Retries
- Idle timeouts
- Circuit breaking
- Common request execution behavior

This means adding a new provider does not require changing the agent state machine.

## 6. Tools — controlled external effects

**Crates:** `crates/harness-tools`, `crates/tools/*`

Tools are the mechanism through which an agent interacts with the outside world.

Examples can include:

- Filesystem access
- Git operations
- Shell commands
- Web requests
- MCP tools
- Skills
- Agent delegation

A model may request a tool, but the core still evaluates whether that request is:

- Allowed
- Denied
- Subject to permission
- Subject to policy restrictions

The runtime then executes the approved tool and sends the result back into the agent loop.

```text
Model requests tool
       ↓
Core checks policy
       ↓
Allowed / denied / permission required
       ↓
Runtime executes tool if allowed
       ↓
Tool result returns to core
```

## 7. Context assembly

**Crate:** `crates/harness-context`

This layer prepares the conversation before it is sent to the model.

It handles the problem that conversations can exceed the model’s context window.

Context providers can:

- Assemble relevant information
- Compact older conversation history
- Apply different compaction tiers
- Add workspace or tool context
- Preserve important state while reducing token usage

The model backend therefore receives an assembled context rather than raw unbounded history.

## 8. Persistence and recovery

**Crate:** `crates/harness-session-store`

Sessions are represented through durable state and events.

The runtime:

- Publishes events to subscribers
- Commits durable events to the session store
- Creates snapshots or checkpoints
- Restores a session from the latest snapshot plus subsequent events

This gives the system restart and recovery behavior.

The important distinction is:

- **Durable events** are persisted.
- **Streaming deltas** are sent to clients but are not necessarily stored individually.

Restoration follows this pattern:

```text
Stored snapshot
   +
Events after snapshot
   ↓
Reconstructed session runtime
```

## 9. Daemon and transports

**Application:** `apps/harnessd`

The daemon exposes the core through external interfaces.

It can use transports such as:

- IPC
- WebSocket
- Stdio
- MCP

The high-level flow is:

```text
Client connection
   ↓
Transport reads a request
   ↓
RPC handler interprets it
   ↓
Harness/session API is called
   ↓
Events or response are returned
```

This allows the same engine to be used either:

- Embedded directly in a Rust application
- Behind a daemon
- Through the TypeScript SDK
- Through desktop applications or other clients

---

# End-to-end example

Suppose a user asks:

> “Find the failing tests and fix them.”

The flow is:

```text
1. Client sends a prompt
2. SessionHandle forwards it to the session runtime
3. Runtime converts it into an AgentCommand
4. harness-core updates the agent state
5. Core returns an ExecuteBackend effect
6. Runtime calls the selected model provider
7. Model requests a filesystem or shell tool
8. Core applies permission and behavior rules
9. Runtime executes the tool
10. Tool result becomes another AgentCommand
11. Core decides whether to call the model again
12. Events are published to the client
13. Durable events are committed to storage
14. Core eventually produces a completion result
```

The important point is that the model is not directly executing tools. The model proposes actions; the core and runtime enforce the rules.

---

# Capacity and scaling

The core has explicit concurrency controls rather than allowing unlimited work.

The default scheduler configuration is:

| Resource | Default capacity |
|---|---:|
| Active sessions | 64 |
| Active agent runners | 256 |
| Agents per session | 32 |
| Concurrent model/backend requests | 8 |
| Concurrent tool executions | 16 |
| Concurrent child processes | 8 |
| Session admission timeout | 5 seconds |
| Tool call timeout | 300 seconds |

The defaults are implemented as `SchedulerConfig::default()`:

```rust
// crates/harness-runtime/src/scheduler/config.rs
SchedulerConfig {
    max_active_sessions: 64,
    max_active_agents: 256,
    max_agents_per_session: 32,
    max_concurrent_backend_requests: 8,
    max_concurrent_tool_executions: 16,
    max_concurrent_processes: 8,
    admission_timeout: Duration::from_secs(5),
    tool_call_timeout: Duration::from_secs(300),
}
```

These are scheduler boundaries, not throughput guarantees. They can be changed by constructing a custom `SchedulerConfig` and passing it to the runtime scheduler. The source is [`crates/harness-runtime/src/scheduler/config.rs`](crates/harness-runtime/src/scheduler/config.rs).

They are configuration defaults, not guaranteed throughput benchmarks.

The runtime uses independent semaphores for:

- Sessions
- Agents
- Backend requests
- Tool executions
- Child processes

This is useful because one resource cannot consume all capacity. For example, a large number of tool executions cannot automatically exhaust the model request capacity.

There are also per-backend limits and rate limiting in the scheduler. That allows the system to apply both:

```text
Global limits
+
Per-provider limits
+
Per-session limits
+
Tool/process limits
```

The runtime also includes:

- Bounded admission waits
- Cancellation
- Tool execution timeouts
- Child-agent depth and budget limits
- Circuit breakers and retry behavior for model backends

So the capacity model is not simply “number of users.” It is a collection of independent resource budgets.

A good way to explain it is:

> The system scales by controlling several bottlenecks independently: sessions, agent loops, model requests, tools, and processes. This prevents one workload type from starving the others.

For actual production capacity, you would still need benchmarks based on:

- Model latency
- Tool duration
- Number of concurrent users
- Event volume
- Persistence backend performance
- CPU and memory
- Provider rate limits
- Average agent steps per request

The repository defines safe concurrency boundaries, but it does not by itself establish a universal requests-per-second number.

---

# Architectural diagram

```text
                    ┌─────────────────────┐
                    │  Clients / SDKs      │
                    │  IDE / Daemon / RPC  │
                    └──────────┬──────────┘
                               │
                    ┌──────────▼──────────┐
                    │  Harness Engine     │
                    │  SessionBuilder     │
                    │  SessionHandle      │
                    └──────────┬──────────┘
                               │
                    ┌──────────▼──────────┐
                    │  Session Runtime    │
                    │  Session manager    │
                    │  Event bus          │
                    │  Persistence        │
                    └──────────┬──────────┘
                               │
             ┌─────────────────▼─────────────────┐
             │          Agent Runner             │
             │  commands → core → effects       │
             └───────────────┬─────────────────┘
                             │
                    ┌────────▼────────┐
                    │  harness-core   │
                    │  Pure state     │
                    │  machine        │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  Effects        │
                    └──────┬─────┬────┘
                           │     │
              ┌────────────▼┐   ┌▼──────────────┐
              │ Model        │   │ Tools         │
              │ integrations │   │ filesystem,   │
              │              │   │ shell, web... │
              └──────────────┘   └───────────────┘
```

With persistence and scheduling alongside the runtime:

```text
                 ┌─────────────────────────┐
                 │ Scheduler / Resource     │
                 │ limits / Cancellation    │
                 └─────────────────────────┘

                 ┌─────────────────────────┐
                 │ Session Store           │
                 │ Events / Snapshots      │
                 └─────────────────────────┘
```

---

# Short version for a non-technical audience

> The core is the brain of the system, but it is intentionally not the hands.
>
> The brain is a pure state machine: it receives events, decides what should happen next, and produces actions. The runtime is the hands: it calls the AI model, runs tools, saves progress, manages concurrency, and sends results back to the brain.
>
> This separation makes the behavior predictable and testable, while allowing the same core to run embedded in an application, behind a daemon, or through different SDKs and transports. Capacity is controlled through independent limits for sessions, agents, model requests, tools, and processes.

# Short version for a technical audience

> `harness-core` implements the deterministic agent transition system. It maps `AgentCommand` values to state transitions and `AgentEffect` values without performing I/O. `harness-runtime` interprets those effects using Tokio, manages sessions and agent runners, invokes `ExecutionBackend` implementations, executes tools, persists events, handles cancellation, and enforces scheduler limits. `harness-engine` composes these layers into the embedding API, while `harness-protocol` defines the shared command/effect/event and RPC vocabulary. Provider integrations, tools, context assembly, persistence, and transports are replaceable adapters around that central loop.

## Primary source references

- `ARCHITECTURE.md`
- `crates/harness-runtime/README.md`
- `crates/harness-runtime/src/scheduler/config.rs`

# Rust Agent Harness — Development Architecture Specification

**Status:** Draft development specification  
**Primary language:** Rust  
**Purpose:** Reusable agent execution platform for IDE embedding, standalone terminal use, daemon/headless execution, and extensibility.

---

## 1. Executive Summary

This project is a reusable Rust agent harness designed around a strict separation between:

1. **Core semantics** — what an agent, session, message, tool capability, usage record, and event are.
2. **Runtime execution** — how agents are scheduled, how backends are called, how tools execute, how cancellation and concurrency work.
3. **Application shells** — IDE, terminal UI, CLI, daemon, test harness, or other clients.

The system must support:

- Multiple sessions executing concurrently.
- A different execution backend per session.
- Different providers/models per session.
- Raw model/API integrations such as Anthropic, OpenAI, Gemini, local/OpenAI-compatible services.
- Higher-level execution integrations such as Claude Code, Codex, or future external agent systems.
- Tools defined at the **agent capability level**, with concrete executors supplied by the runtime.
- Per-agent usage, context, cost, and budget accounting.
- Subagents with inherited or explicitly overridden backend, tools, workspace policy, and budget.
- Rich live event streaming for IDE/TUI use, including model status, text streaming, provider-exposed reasoning, tool calls, progress, subagent activity, and usage.
- Local embedded use with no transport.
- Sidecar/daemon use over WebSocket, IPC, stdio, JSON-RPC, or another transport.
- Third-party extensions without requiring changes to core agent logic.

The fundamental design rule is:

> **The core defines contracts and deterministic agent semantics. The runtime executes effects. Applications observe events and send commands. Integrations implement stable interfaces.**

---

# 2. Design Goals

## 2.1 Primary goals

### Portable

The same agent/session implementation must work in:

- a native IDE,
- a terminal UI,
- a CLI,
- a local daemon,
- a sidecar,
- a remote service,
- integration tests.

Application-specific concerns must not leak into core agent execution.

### Concurrent

The runtime must support:

- many sessions at once,
- many agents per session,
- subagent trees,
- concurrent model calls,
- concurrent tool calls where allowed,
- independent cancellation and failure isolation.

### Backend-neutral

A session may use:

- Anthropic API,
- OpenAI API,
- Gemini,
- OpenRouter,
- Ollama,
- another OpenAI-compatible endpoint,
- Claude Code,
- Codex,
- a custom internal backend,
- a backend supplied directly by an embedding application.

The core must never require provider-specific branches.

### Capability-oriented

Agent capabilities must be explicit.

An agent sees only the tools and capabilities granted to that agent.

The runtime may know about 100 installed tools while a specific agent sees only 5.

### Observable

Every meaningful stage of execution must be observable through normalized events.

Frontends must be able to display:

- current status,
- streaming assistant output,
- provider-exposed reasoning/thinking content,
- tool calls,
- tool progress/output,
- permission requests,
- subagents,
- failures,
- usage,
- completion.

### Extensible

Integrations and tools must be addable without modifying the agent core.

---

# 3. Non-Goals

The initial architecture should **not** assume:

- Tauri.
- WebSocket.
- Ratatui.
- a particular model provider.
- a particular database.
- MCP as the only extension protocol.
- Rust dynamic libraries as the primary plugin ABI.
- a globally selected model.
- one process per session.
- one agent per session.
- one workspace model.
- that every backend exposes exact token usage or reasoning data.

---

# 4. Conceptual Architecture

```text
┌──────────────────────────────────────────────────────────────┐
│                         APPLICATIONS                         │
│                                                              │
│      IDE        TUI        CLI        Daemon        Tests    │
└─────────────────────────────┬────────────────────────────────┘
                              │
                     Commands / Events
                              │
┌─────────────────────────────▼────────────────────────────────┐
│                        HARNESS ENGINE                        │
│                                                              │
│  ergonomic public API                                        │
│  session creation                                            │
│  registration                                                │
│  capability discovery                                        │
└─────────────────────────────┬────────────────────────────────┘
                              │
┌─────────────────────────────▼────────────────────────────────┐
│                           RUNTIME                            │
│                                                              │
│  SessionManager        AgentSupervisor                       │
│  Scheduler             ResourceManager                       │
│  ToolRegistry          IntegrationRegistry                   │
│  ExtensionManager      SessionStore                          │
│  cancellation          execution                             │
└─────────────────────────────┬────────────────────────────────┘
                              │
                    owns/executes instances
                              │
┌─────────────────────────────▼────────────────────────────────┐
│                            CORE                              │
│                                                              │
│  Session domain      Agent domain                            │
│  Messages            Events / Commands                       │
│  Effects             Usage / Budgets                         │
│  Tool capabilities   Backend contracts                       │
│  Context contracts   IDs / protocol types                    │
└──────────────────────────────────────────────────────────────┘
```

A useful mental model is:

> **Core = semantics**  
> **Runtime = execution**  
> **Engine = public embedding API**  
> **Frontend = presentation/control**

---

# 5. Main Domain Entities

The primary entities are:

```text
Harness
  └── Session
       ├── Root Agent
       │    ├── Subagent
       │    └── Subagent
       └── session environment
```

The most important distinctions are:

| Entity | Responsibility |
|---|---|
| `Harness` | owns runtime-wide services and registries |
| `Session` | user-facing execution scope |
| `Agent` | one independent reasoning worker |
| `Run` | one active execution of an agent |
| `ExecutionBackend` | performs model/agent execution |
| `ToolCapability` | declares what a specific agent may use |
| `ToolExecutor` | performs actual tool I/O |
| `Workspace` | abstract project/filesystem environment |

---

# 6. Session

A **Session** is the user-facing conversation/execution boundary.

Examples:

- IDE chat tab A
- IDE chat tab B
- terminal conversation
- API task session

Sessions execute independently.

```text
Runtime
 ├── Session A → Anthropic
 ├── Session B → Claude Code
 ├── Session C → Codex
 └── Session D → Local model
```

Each session can have a completely different backend and model.

## 6.1 Session responsibilities

A session owns or references:

- identity,
- root agent,
- workspace,
- injected backend binding,
- session metadata,
- persistence identity,
- session event aggregation,
- session-level usage aggregation,
- child agent hierarchy.

It should **not** contain provider-specific code.

## 6.2 Suggested types

```rust
pub struct SessionId(Uuid);
pub struct AgentId(Uuid);
pub struct RunId(Uuid);

pub struct SessionState {
    pub id: SessionId,
    pub root_agent_id: AgentId,
    pub metadata: SessionMetadata,
}

pub struct SessionRuntime {
    pub state: SessionState,

    // Runtime-only dependency.
    pub default_backend: Arc<dyn ExecutionBackend>,

    pub workspace: Arc<dyn Workspace>,
    pub event_bus: SessionEventBus,
}
```

The persistent `SessionState` and live `SessionRuntime` should remain conceptually separate because runtime dependencies such as `Arc<dyn ExecutionBackend>` are not directly serializable.

---

# 7. Agent

An **Agent** is a first-class entity.

A session has a root agent. Subagents are also full agents.

Do not create a separate reduced `SubAgent` abstraction.

```text
Session A
   │
   └── Agent A
        ├── Agent A1
        └── Agent A2
             └── Agent A2.1
```

## 7.1 Agent responsibilities

An agent owns:

- identity,
- parent relationship,
- state,
- transcript/message history,
- tool capabilities,
- usage ledger,
- budget,
- backend binding metadata,
- current run,
- child relationships.

The agent does **not** directly perform network requests or operating-system I/O.

## 7.2 Suggested type

```rust
pub struct Agent {
    pub id: AgentId,
    pub session_id: SessionId,
    pub parent_id: Option<AgentId>,

    pub state: AgentState,

    pub backend: BackendBinding,
    pub capabilities: AgentCapabilities,

    pub usage: UsageLedger,
    pub budget: AgentBudget,
}
```

`BackendBinding` is an identity/configuration reference used by the core. The concrete backend implementation remains a runtime dependency.

---

# 8. Agent State

```rust
pub struct AgentState {
    pub status: AgentStatus,

    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,

    pub active_run: Option<RunId>,
    pub pending_tools: HashMap<ToolCallId, PendingToolCall>,

    pub children: Vec<AgentId>,

    pub last_error: Option<AgentError>,
}
```

Suggested status representation:

```rust
pub enum AgentStatus {
    Idle,
    PreparingContext,
    WaitingForBackend,
    Streaming,
    Executing,
    WaitingForPermission,
    WaitingForChildren,
    Paused,
    Completed,
    Cancelled,
    Failed,
}
```

A separate current operation is preferable to encoding every execution detail into `AgentStatus`.

```rust
pub enum AgentOperation {
    BackendRequest {
        request_id: RequestId,
    },

    Tools {
        calls: Vec<ToolCallId>,
    },

    Children {
        agents: Vec<AgentId>,
    },

    Permission {
        request_id: PermissionId,
    },
}
```

This allows the frontend to display meaningful status without creating hundreds of status variants.

---

# 9. Core Transition Model

The core should be as deterministic as practical.

Conceptually:

```text
Command
   ↓
Agent state transition
   ↓
Effects
```

Suggested API:

```rust
pub impl Agent {
    pub fn apply(
        &mut self,
        command: AgentCommand,
    ) -> Vec<AgentEffect>;
}
```

## 9.1 Commands

```rust
pub enum AgentCommand {
    StartRun(UserInput),

    BackendEvent {
        run_id: RunId,
        event: ExecutionEvent,
    },

    ToolCompleted {
        call_id: ToolCallId,
        result: ToolResult,
    },

    ToolFailed {
        call_id: ToolCallId,
        error: ToolError,
    },

    PermissionResolved {
        id: PermissionId,
        decision: PermissionDecision,
    },

    ChildCompleted {
        agent_id: AgentId,
        result: AgentResult,
    },

    ChildFailed {
        agent_id: AgentId,
        error: AgentError,
    },

    Cancel,
    Pause,
    Resume,
}
```

## 9.2 Effects

```rust
pub enum AgentEffect {
    ExecuteBackend {
        request: ExecutionRequest,
    },

    ExecuteTool {
        request: ToolRequest,
    },

    SpawnAgent {
        spec: SpawnAgentSpec,
    },

    RequestPermission {
        request: PermissionRequest,
    },

    Persist {
        mutation: SessionMutation,
    },

    Emit {
        event: AgentEvent,
    },

    FinishRun {
        result: AgentResult,
    },
}
```

The runtime interprets effects.

The core does not execute them.

---

# 10. Agent Runner

Keep the durable `Agent` separate from execution machinery.

```rust
pub struct AgentRunner {
    // references/handles, not provider-specific logic
}
```

Responsibilities:

- deliver commands to the agent,
- interpret effects,
- call the execution backend,
- execute tools through the tool runtime,
- emit runtime events,
- handle cancellation,
- supervise active work.

Conceptually:

```text
Agent
 ├── durable identity
 ├── state
 ├── transcript
 ├── usage
 ├── capabilities
 └── budget

AgentRunner
 ├── async loop
 ├── backend call
 ├── tool dispatch
 ├── cancellation
 └── event forwarding
```

This prevents `Agent` from becoming a monolithic object containing state, networking, tools, persistence, and scheduling.

---

# 11. Execution Backend

The primary abstraction should be **`ExecutionBackend`**, not simply `Provider`.

This is important because these are not equivalent execution systems:

```text
Anthropic API
OpenAI API
Gemini API
local OpenAI-compatible API
Claude Code
Codex
custom company agent
```

Some are raw model APIs.

Others may own part or all of an agent loop.

The harness should normalize them behind one high-level interface.

## 11.1 Contract

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;

    fn capabilities(&self) -> BackendCapabilities;

    async fn execute(
        &self,
        request: ExecutionRequest,
        context: ExecutionContext,
        events: ExecutionEventSink,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError>;
}
```

The backend implementation owns its provider-specific configuration.

The core never knows:

- API key representation,
- OAuth details,
- request endpoint,
- executable path,
- Claude Code permission syntax,
- Codex process protocol,
- model-specific request JSON.

---

# 12. Backend Per Session

The runtime must allow direct backend injection when a session is created.

```rust
let session_a = harness
    .session()
    .backend(Arc::new(AnthropicBackend::new(config_a)))
    .tools(toolset_a)
    .start()
    .await?;

let session_b = harness
    .session()
    .backend(Arc::new(ClaudeCodeBackend::new(config_b)))
    .tools(toolset_b)
    .start()
    .await?;

let session_c = harness
    .session()
    .backend(Arc::new(CodexBackend::new(config_c)))
    .tools(toolset_c)
    .start()
    .await?;
```

These sessions can execute concurrently.

There should be no global active model or global active provider.

---

# 13. Generic Model Backend

Do not duplicate the entire harness loop for ordinary API providers.

Provide a generic model backend.

```text
ExecutionBackend
       │
       ├── GenericModelBackend
       │        │
       │        └── ModelClient
       │             ├── Anthropic
       │             ├── OpenAI
       │             ├── Gemini
       │             ├── OpenRouter
       │             └── OpenAI-compatible
       │
       ├── ClaudeCodeBackend
       ├── CodexBackend
       └── CustomBackend
```

Suggested low-level model client:

```rust
#[async_trait]
pub trait ModelClient: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;

    async fn stream(
        &self,
        request: ModelRequest,
        events: ModelEventSink,
        cancel: CancellationToken,
    ) -> Result<ModelResult, ModelError>;
}
```

The generic backend owns the harness-controlled agent loop for raw model providers.

Specialized backends can own their own execution semantics where necessary.

---

# 14. Backend Capabilities

Never branch on backend identity when capability detection is sufficient.

Avoid:

```rust
if backend.name() == "claude-code" { ... }
```

Prefer:

```rust
if backend.capabilities().resumable_sessions {
    ...
}
```

Suggested capabilities:

```rust
pub struct BackendCapabilities {
    pub streaming: bool,
    pub reasoning_stream: bool,

    pub tool_calls: bool,
    pub parallel_tool_calls: bool,

    pub host_managed_tools: bool,
    pub backend_managed_tools: bool,

    pub permissions: bool,
    pub images: bool,

    pub resumable_sessions: bool,
    pub native_subagents: bool,
    pub model_switching: bool,

    pub exact_usage: bool,
    pub exact_cost: bool,
}
```

---

# 15. Backend Identity and Persistence

Do not persist live backend objects.

Persist a reference:

```rust
pub struct BackendReference {
    pub integration: IntegrationId,
    pub configuration: ConfigurationId,
    pub model: Option<ModelId>,
}
```

Example:

```text
integration   = "anthropic"
configuration = "work-account"
model         = "claude-..."
```

Restoration flow:

```text
Persistent Session
      ↓
BackendReference
      ↓
IntegrationRegistry
      ↓
IntegrationFactory
      ↓
Arc<dyn ExecutionBackend>
      ↓
SessionRuntime
```

Credentials remain outside conversation/session history.

---

# 16. Integration Registry

For dynamic configuration, provide factories.

```rust
#[async_trait]
pub trait IntegrationFactory: Send + Sync {
    fn id(&self) -> IntegrationId;

    fn descriptor(&self) -> IntegrationDescriptor;

    async fn create(
        &self,
        config: IntegrationConfig,
    ) -> Result<Arc<dyn ExecutionBackend>, IntegrationError>;
}
```

Runtime registry:

```rust
pub struct IntegrationRegistry {
    integrations: HashMap<IntegrationId, Arc<dyn IntegrationFactory>>,
}
```

Support both:

### Direct library injection

```rust
.session()
.backend(my_backend)
```

### Registry/configuration construction

```rust
.session()
.integration("anthropic", config)
```

This provides flexibility for both IDE embedding and standalone applications.

---

# 17. Tools

Tools are defined at the **agent capability level**.

The agent determines which tools are visible and usable.

The runtime owns concrete executors.

This distinction is fundamental:

> **Tool availability belongs to the Agent. Tool implementation belongs to the Runtime.**

## 17.1 Tool descriptor

```rust
pub struct ToolDescriptor {
    pub id: ToolId,
    pub name: String,
    pub description: String,
    pub input_schema: JsonSchema,
}
```

## 17.2 Tool capability

```rust
pub struct ToolCapability {
    pub descriptor: ToolDescriptor,
    pub policy: ToolPolicy,

    /// Whether this capability may be delegated to a child agent.
    pub delegatable: bool,
}
```

```rust
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

pub struct ToolPolicy {
    pub permission: PermissionMode,
    pub enabled: bool,
}
```

## 17.3 Agent toolset

```rust
pub struct AgentToolset {
    pub tools: HashMap<ToolId, ToolCapability>,
}
```

This toolset is injected when the root agent is created from the session configuration.

---

# 18. Tool Executor Registry

The runtime keeps actual executors:

```rust
pub struct ToolRegistry {
    executors: HashMap<ToolId, Arc<dyn ToolExecutor>>,
}
```

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;

    async fn execute(
        &self,
        context: ToolExecutionContext,
        arguments: serde_json::Value,
        events: ToolEventSink,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}
```

Execution flow:

```text
Model requests "shell"
        ↓
Agent verifies tool capability
        ↓
Permission policy
        ↓
Runtime ToolRegistry
        ↓
ShellExecutor
        ↓
Tool events/result
        ↓
Agent
```

---

# 19. Tool Injection During Session Creation

Example:

```rust
let session = harness
    .session()
    .backend(backend)
    .workspace(workspace)
    .tools([
        ToolCapability::allow("fs.read"),
        ToolCapability::allow("workspace.search"),
        ToolCapability::ask("fs.edit"),
        ToolCapability::ask("shell.exec"),
        ToolCapability::deny("git.push"),
    ])
    .start()
    .await?;
```

Internally:

```text
SessionBuilder
     │
     ├── creates Session
     │
     └── creates Root Agent
              │
              └── injects AgentToolset
```

The model request must obtain tools from the agent:

```rust
let tools = agent
    .capabilities
    .tools
    .enabled_descriptors();
```

Never advertise the complete runtime tool registry to every agent.

---

# 20. Agent Capabilities

Tools will not be the only capability.

Use a larger capability structure:

```rust
pub struct AgentCapabilities {
    pub tools: AgentToolset,

    pub can_spawn_agents: bool,
    pub max_child_depth: Option<u32>,

    pub workspace: WorkspaceCapabilities,

    pub backend: BackendCapabilities,
}
```

This creates one central place to inspect what an agent is allowed and able to do.

---

# 21. Subagents

Subagents are normal `Agent` entities.

A parent emits a spawn effect.

```rust
AgentEffect::SpawnAgent {
    spec: SpawnAgentSpec,
}
```

The runtime:

1. creates an `AgentId`,
2. resolves backend policy,
3. resolves tool inheritance,
4. resolves workspace policy,
5. applies budget,
6. creates cancellation scope,
7. registers parent/child relationship,
8. spawns the child runner.

---

# 22. Subagent Specification

```rust
pub struct SpawnAgentSpec {
    pub role: Option<String>,

    pub backend: BackendPolicy,
    pub tools: ToolInheritance,
    pub workspace: WorkspacePolicy,

    pub budget: AgentBudget,

    pub mode: SpawnMode,
}
```

Backend policy:

```rust
pub enum BackendPolicy {
    Inherit,
    Explicit(BackendReference),
}
```

Tool inheritance:

```rust
pub enum ToolInheritance {
    InheritAll,
    Subset(Vec<ToolId>),
    Replace(AgentToolset),
}
```

Workspace policy:

```rust
pub enum WorkspacePolicy {
    Inherit,
    ReadOnly,
    Snapshot,
    NewWorktree,
}
```

Execution mode:

```rust
pub enum SpawnMode {
    AwaitResult,
    Concurrent,
}
```

---

# 23. Capability Non-Escalation

A child must not automatically obtain capabilities that its parent cannot delegate.

Default invariant:

```text
ChildTools ⊆ ParentDelegatableTools
```

An external authority may explicitly grant more capability, but this must be an explicit runtime/user policy decision.

Example:

```text
Root Agent
  fs.read       allowed + delegatable
  fs.edit       allowed + not delegatable
  shell.exec    ask    + not delegatable

Research Agent
  fs.read
```

The research agent cannot silently grant itself `shell.exec`.

---

# 24. Heterogeneous Agent Trees

A child may inherit or override the parent's backend.

This allows:

```text
Root Agent
Claude
   │
   ├── Research Agent
   │      Gemini
   │
   └── Implementation Agent
          Codex
```

The runtime and event protocol must not assume one provider/model per session tree.

Usage records therefore store backend/model identity per execution record.

---

# 25. Run

A session persists across many user turns.

An agent persists across many runs.

A `Run` is one active execution.

```text
Agent A
 ├── Run 1 — "investigate bug"
 ├── Run 2 — "apply fix"
 └── Run 3 — "run tests"
```

Suggested run state:

```rust
pub struct Run {
    pub id: RunId,
    pub agent_id: AgentId,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub status: RunStatus,
}
```

This makes cancellation, diagnostics, usage attribution, and replay easier.

---

# 26. Usage Accounting

Usage is a core agent concern.

Do not store only one cumulative integer.

Use a ledger.

```rust
pub struct UsageLedger {
    pub records: Vec<UsageRecord>,
}
```

```rust
pub struct UsageRecord {
    pub run_id: RunId,
    pub request_id: Option<RequestId>,

    pub backend: BackendId,
    pub integration: IntegrationId,
    pub model: Option<ModelId>,

    pub usage: ModelUsage,
    pub cost: Cost,

    pub timestamp: Timestamp,
}
```

---

# 27. Model Usage

Not every integration provides every metric.

Unknown must not mean zero.

```rust
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,

    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,

    pub reasoning_tokens: Option<u64>,

    pub total_tokens: Option<u64>,
}
```

Semantics:

```text
Some(0) = backend explicitly reported zero
None    = backend does not expose this metric
```

---

# 28. Context Usage vs Cumulative Usage

These metrics must remain separate.

Example:

```text
Request 1: 10k input
Request 2: 14k input
Request 3: 18k input
```

Cumulative input consumption:

```text
42k
```

Current model context:

```text
approximately 18k
```

These are not equivalent.

Suggested structure:

```rust
pub struct AgentUsageMetrics {
    pub cumulative: CumulativeUsage,
    pub current_context: ContextUsage,
    pub current_run: RunUsage,
}
```

Frontends can then display:

```text
Context      82k / 200k
Current run  37k tokens
Agent total  412k tokens
Cost         $...
```

---

# 29. Cost

Some backends report exact cost, some expose enough data to calculate it, and some expose nothing meaningful.

```rust
pub enum CostSource {
    ProviderReported,
    Calculated,
    Estimated,
}

pub struct Cost {
    pub amount_usd: Option<Decimal>,
    pub source: Option<CostSource>,
}
```

Do not imply exact billing information when it is not available.

---

# 30. Agent Budgets

```rust
pub struct AgentBudget {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_total_tokens: Option<u64>,

    pub max_cost_usd: Option<Decimal>,

    pub max_requests: Option<u64>,
    pub max_tool_calls: Option<u64>,

    pub max_children: Option<u32>,
    pub max_depth: Option<u32>,
}
```

Subagents can receive tighter budgets than the root agent.

---

# 31. Usage Tree

Usage should support:

- self usage,
- descendant usage,
- inclusive usage.

```rust
pub struct AgentUsageSummary {
    pub self_usage: Usage,
    pub descendant_usage: Usage,
    pub inclusive_usage: Usage,
}
```

Do not modify the root agent's direct usage every time a child spends tokens. Aggregate when needed to avoid double counting.

---

# 32. Runtime Concurrency Model

The runtime should be actor-like.

Each independently executing agent has:

- a mailbox,
- mutable state,
- event output,
- cancellation token,
- child handles.

```rust
pub struct AgentTask {
    pub id: AgentId,
    pub commands: mpsc::Receiver<AgentCommand>,
    pub events: broadcast::Sender<AgentEventEnvelope>,
    pub cancel: CancellationToken,
}
```

The core should not "run sessions in parallel."

More precise rule:

> **The core supports independent agent state machines. The runtime executes them concurrently.**

---

# 33. Session Manager and Agent Supervisor

Recommended hierarchy:

```text
Runtime
 │
 └── SessionManager
       │
       ├── Session A
       │    └── AgentSupervisor
       │         ├── Root
       │         ├── Research
       │         └── Tests
       │
       └── Session B
            └── AgentSupervisor
                 └── Root
```

`SessionManager` responsibilities:

- create sessions,
- close sessions,
- restore sessions,
- expose session handles.

`AgentSupervisor` responsibilities:

- spawn agents,
- track parent/child relationships,
- propagate cancellation,
- isolate failures,
- clean up completed agents,
- enforce child/depth limits.

---

# 34. Scheduler

Once multiple sessions and subagents are possible, global resource control is required.

```rust
pub struct SchedulerConfig {
    pub max_active_sessions: usize,
    pub max_active_agents: usize,
    pub max_agents_per_session: usize,

    pub max_concurrent_backend_requests: usize,
    pub max_concurrent_tool_executions: usize,
    pub max_concurrent_processes: usize,
}
```

Provider-specific limits may also exist:

```rust
pub struct BackendRateLimits {
    pub max_concurrent_requests: usize,
    pub requests_per_minute: Option<u32>,
    pub tokens_per_minute: Option<u64>,
}
```

Use semaphores/permits rather than unrestricted task spawning.

---

# 35. Cancellation

Cancellation must be hierarchical.

```text
Session token
   │
   └── Root agent token
         ├── Child A token
         │     └── Child A1 token
         └── Child B token
```

Cancel session:

```text
session
 ↓
root
 ↓
children
 ↓
backend streams
 ↓
tools/processes
```

Cancel one child:

```text
Root continues
Child A continues
Child B cancelled
```

Cancellation semantics should be explicit and testable.

---

# 36. Failure Isolation

One failed session must not crash unrelated sessions.

One failed child must not automatically crash its parent.

```text
Runtime
 ├── Session A ✔
 ├── Session B failed
 └── Session C ✔
```

Parent receives:

```rust
AgentCommand::ChildFailed {
    agent_id,
    error,
}
```

The parent/core then decides how that failure influences reasoning.

---

# 37. Workspace

The workspace must be injected.

Do not hardcode direct `std::fs` access into agent logic.

```rust
#[async_trait]
pub trait Workspace: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, WorkspaceError>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), WorkspaceError>;
    async fn search(&self, query: SearchQuery) -> Result<SearchResult, WorkspaceError>;

    async fn status(&self) -> Result<WorkspaceStatus, WorkspaceError>;
}
```

Implementations may include:

```text
FsWorkspace
IdeWorkspace
RpcWorkspace
ReadOnlyWorkspace
WorktreeWorkspace
```

An IDE implementation may surface unsaved buffers rather than stale disk content.

---

# 38. Workspace Isolation

Parallel sessions may share or isolate project state.

Support at least:

```rust
pub enum WorkspaceMode {
    Shared,
    Isolated,
}
```

Potential implementation:

```text
Session A → /project
Session B → /project
```

or:

```text
Session A → worktree/session-a
Session B → worktree/session-b
```

Subagents may also use:

- inherited workspace,
- read-only workspace,
- snapshot,
- new git worktree.

---

# 39. Resource Manager

Concurrent sessions can conflict.

Example:

```text
Session A writes src/main.rs
Session B writes src/main.rs
```

Runtime should have a place to coordinate this.

```rust
pub enum ResourceKey {
    File(PathBuf),
    GitRepository(PathBuf),
    Workspace(WorkspaceId),
    Terminal(TerminalId),
    Custom(String),
}

pub enum AccessMode {
    Shared,
    Exclusive,
}
```

Not every implementation must use pessimistic locking, but conflict management must have an architectural home.

---

# 40. Event System

Observability is a first-class feature.

Agents emit normalized events.

Sessions aggregate events from all agents.

Applications subscribe.

```text
Agent
  ↓
AgentEvent
  ↓
SessionEventBus
  ├── IDE
  ├── TUI
  ├── WebSocket
  ├── logging
  └── telemetry
```

---

# 41. Agent Events

Suggested events:

```rust
pub enum AgentEvent {
    StateChanged {
        from: AgentStatus,
        to: AgentStatus,
    },

    RunStarted {
        run_id: RunId,
    },

    BackendRequestStarted {
        request_id: RequestId,
    },

    AssistantMessageStarted {
        message_id: MessageId,
    },

    AssistantTextDelta {
        message_id: MessageId,
        delta: String,
    },

    ReasoningDelta {
        message_id: MessageId,
        delta: String,
    },

    AssistantMessageCompleted {
        message_id: MessageId,
    },

    ToolCallRequested {
        call: ToolCall,
    },

    ToolCallStarted {
        call_id: ToolCallId,
    },

    ToolCallProgress {
        call_id: ToolCallId,
        progress: ToolProgress,
    },

    ToolCallCompleted {
        call_id: ToolCallId,
        result: ToolResultSummary,
    },

    PermissionRequested {
        request: PermissionRequest,
    },

    UsageUpdated {
        usage: AgentUsageSnapshot,
    },

    ChildAgentSpawned {
        agent_id: AgentId,
    },

    ChildAgentCompleted {
        agent_id: AgentId,
        outcome: AgentOutcome,
    },

    Failed {
        error: AgentError,
    },

    Completed {
        outcome: AgentOutcome,
    },
}
```

---

# 42. Reasoning / Thinking Events

The protocol may expose **provider-supplied, user-visible reasoning content** when a backend supports it.

The architecture must not assume hidden chain-of-thought is available.

```rust
BackendCapabilities {
    reasoning_stream: true,
    ...
}
```

A backend without such a stream simply does not emit `ReasoningDelta`.

Frontends should treat this as optional.

Useful visible execution information is broader than reasoning:

- current phase,
- tool activity,
- files being read,
- commands running,
- subagent status,
- progress,
- model output.

---

# 43. Event Envelope

Every event requires identity and ordering metadata.

```rust
pub struct AgentEventEnvelope {
    pub event_id: EventId,

    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub parent_agent_id: Option<AgentId>,
    pub run_id: Option<RunId>,

    pub agent_sequence: u64,
    pub session_sequence: Option<u64>,

    pub timestamp: Timestamp,

    pub event: AgentEvent,
}
```

Do not rely on timestamps alone for ordering concurrent execution.

---

# 44. Session Events

The session is the preferred frontend subscription boundary.

```rust
pub enum SessionEvent {
    SessionStarted,

    StatusChanged {
        status: SessionStatus,
    },

    Agent {
        event: AgentEventEnvelope,
    },

    AgentAdded {
        agent: AgentDescriptor,
    },

    AgentRemoved {
        agent_id: AgentId,
    },

    UsageUpdated {
        usage: SessionUsageSnapshot,
    },

    Completed,

    Failed {
        error: SessionError,
    },
}
```

A frontend subscribes to one session and automatically receives events for the root agent and descendants.

---

# 45. Snapshot + Stream

Expose both:

```rust
session.snapshot()
session.subscribe()
```

A snapshot answers:

> What is true now?

The event stream answers:

> What changed?

Suggested snapshot:

```rust
pub struct SessionSnapshot {
    pub id: SessionId,
    pub status: SessionStatus,

    pub root_agent_id: AgentId,
    pub agents: Vec<AgentSnapshot>,

    pub usage: SessionUsageSnapshot,
}
```

```rust
pub struct AgentSnapshot {
    pub id: AgentId,
    pub parent_id: Option<AgentId>,

    pub status: AgentStatus,
    pub current_operation: Option<AgentOperation>,

    pub backend: BackendDescriptor,
    pub usage: AgentUsageSnapshot,
}
```

This is essential for reconnectable frontends.

---

# 46. Durable vs Ephemeral Events

Do not persist every token delta.

### Durable

Persist events such as:

- message completed,
- tool started/completed,
- agent spawned/completed,
- permission decisions,
- backend/model changes,
- usage records,
- errors,
- relevant state transitions.

### Ephemeral

Normally do not persist every:

- text chunk,
- reasoning chunk,
- stdout byte chunk,
- spinner/progress update.

Persist the final assembled message/tool output instead.

---

# 47. Event Visibility

Use visibility levels so debug events do not automatically appear in normal UI.

```rust
pub enum EventVisibility {
    User,
    Developer,
    Internal,
}
```

Examples:

```text
assistant output       User
tool execution         User
usage                  User/Developer
scheduler permit       Developer
HTTP retry             Internal
auth header            NEVER EMIT
```

Event streams must never leak secrets.

---

# 48. Session API

Prefer streams/subscriptions over callback proliferation.

Public shape:

```rust
pub trait SessionClient {
    async fn send(
        &self,
        command: SessionCommand,
    ) -> Result<(), SessionError>;

    async fn snapshot(
        &self,
    ) -> Result<SessionSnapshot, SessionError>;

    fn subscribe(
        &self,
    ) -> SessionEventStream;
}
```

Useful session commands:

```rust
pub enum SessionCommand {
    Prompt(UserInput),

    CancelRun {
        run_id: RunId,
    },

    CancelAgent {
        agent_id: AgentId,
    },

    PauseAgent {
        agent_id: AgentId,
    },

    ResumeAgent {
        agent_id: AgentId,
    },

    ApprovePermission {
        id: PermissionId,
    },

    RejectPermission {
        id: PermissionId,
    },
}
```

---

# 49. Local and Remote Session Clients

Transport must be separate from session semantics.

```text
Session API
    │
    ├── LocalSessionClient
    │       └── direct channels/function calls
    │
    └── RemoteSessionClient
            └── WebSocket / IPC / RPC
```

A frontend should ideally not care which implementation it received.

This allows the current sidecar architecture to coexist with a future fully local/native build.

---

# 50. Transport Layer

Transport adapters may include:

```text
WebSocket
stdio
Unix socket
named pipe
JSON-RPC
MessagePack RPC
QUIC
custom application IPC
```

Transport is only responsible for carrying commands, snapshots, and events.

It must not implement agent semantics.

---

# 51. Deployment Models

The same engine should support multiple deployment forms.

## 51.1 Standalone TUI

```text
Ratatui
   │
Harness Engine
   │
local integrations/tools
```

## 51.2 Native IDE embedded

```text
IDE process
 ├── editor
 ├── language services
 ├── Harness Engine
 └── IDE-specific tools
```

No WebSocket is required.

## 51.3 Sidecar

```text
Frontend
   │
WebSocket / IPC
   │
Harness sidecar
   │
Harness Engine
```

## 51.4 Daemon

```text
IDE / CLI / other client
        │
       RPC
        │
    harnessd
        │
 Harness Engine
```

## 51.5 Tests

```text
Test
 │
Harness Engine
 ├── FakeBackend
 ├── FakeTools
 ├── FakeWorkspace
 └── deterministic event assertions
```

---

# 52. Extensions

Extension architecture should not be equivalent to `Tool`.

Potential extension points include:

- tools,
- execution backends,
- model clients,
- context providers,
- commands,
- event observers,
- lifecycle interceptors,
- workspace providers,
- session metadata,
- policy providers.

---

# 53. Extension Registry

```rust
pub struct ExtensionRegistry {
    pub tools: ToolRegistry,
    pub integrations: IntegrationRegistry,
    pub context_providers: ContextProviderRegistry,
    pub commands: CommandRegistry,
    pub observers: ObserverRegistry,
    pub interceptors: InterceptorRegistry,
}
```

Extensions register capabilities. They do not automatically grant those capabilities to every agent.

Example:

```text
GitHub extension
     ↓
register github.search
     ↓
runtime knows tool exists
     ↓
session config grants github.search
     ↓
specific agent sees tool
```

---

# 54. Observers vs Interceptors

Do not conflate passive observation with behavior modification.

### Observer

```rust
async fn on_tool_finished(&self, event: &ToolResult);
```

Cannot alter execution.

### Interceptor

```rust
async fn before_tool(
    &self,
    request: ToolRequest,
) -> Result<ToolRequest, InterceptorError>;
```

May alter, deny, or enrich execution.

This distinction is important for predictability and debugging.

---

# 55. Plugin ABI Strategy

Avoid making Rust `cdylib` plugins the primary third-party extension model.

Rust's ABI stability makes long-lived binary plugin compatibility difficult.

Recommended layers:

```text
Extensions
  ├── linked Rust crates
  ├── subprocess / JSON-RPC plugins
  ├── WASM plugins
  └── MCP adapter
```

Initial implementation can start with:

1. built-in/linked Rust extensions,
2. subprocess tool extensions,
3. MCP compatibility,
4. WASM later if needed.

---

# 56. Context Engine

Context construction should be pluggable.

```rust
#[async_trait]
pub trait ContextProvider: Send + Sync {
    fn id(&self) -> ContextProviderId;

    async fn provide(
        &self,
        request: ContextRequest,
    ) -> Result<Vec<ContextItem>, ContextError>;
}
```

Potential providers:

```text
ProjectRulesContext
GitContext
OpenFilesContext
SelectionContext
DiagnosticsContext
MemoryContext
ExtensionContext
```

The standalone build may expose:

```text
filesystem
git
shell
```

The IDE may additionally expose:

```text
open buffers
selection
LSP diagnostics
references
definitions
debug state
```

The agent does not need to know where the context came from.

---

# 57. Context Pipeline

Conceptually:

```text
System instructions
       +
conversation
       +
project rules
       +
workspace context
       +
IDE context
       +
tool definitions
       +
extension context
       ↓
Context Engine
       ↓
ExecutionRequest
```

Suggested transforms:

```text
ProjectInstructions
HistoryCompaction
TokenBudget
OpenEditorContext
GitContext
DiagnosticsContext
ToolDefinitionInjection
```

---

# 58. Context Budget and Compaction

Compaction must be a first-class runtime feature.

```rust
pub struct ContextBudget {
    pub max_tokens: usize,
    pub reserved_output_tokens: usize,
    pub compaction_threshold: f32,
}
```

Check the budget between backend/tool iterations, not just between user messages.

```text
tool completes
    ↓
estimate next context
    ↓
within budget? ── yes → continue
    │
    no
    ↓
compact
    ↓
continue
```

---

# 59. Persistence

Prefer an append-oriented durable session history.

Possible storage implementations:

```text
SQLite
JSONL
application-managed database
remote store
```

The storage contract should be abstract:

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession>;
    async fn append(&self, event: DurableSessionEvent) -> Result<()>;
    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<()>;
}
```

SQLite is a strong default for desktop applications.

---

# 60. Transcript Invariants

Provider transcript requirements can be strict.

Enforce structural invariants before persistence and backend submission.

Example:

```text
assistant(tool_call)
       ↓
tool_result
       ↓
next conversational message
```

Do not permit invalid branches that leave unresolved tool calls unless the protocol explicitly supports it.

Validation should be centralized.

---

# 61. Security and Permissions

Tool authorization must be distinct from tool execution.

Flow:

```text
tool request
    ↓
agent capability check
    ↓
policy check
    ↓
allow / ask / deny
    ↓
executor
```

Potential policy sources:

- agent tool policy,
- session policy,
- workspace policy,
- extension policy,
- application/user approval.

A root agent having access to a capability does not imply that every child can delegate it.

---

# 62. Secret Handling

Secrets must not enter:

- normal events,
- session transcript,
- debug logs,
- extension payloads without explicit policy.

Keep provider credentials in integration configuration/secret stores.

Persist only safe references where possible.

---

# 63. Public Harness API

The ergonomic application-facing API should be intentionally small.

Example:

```rust
let harness = Harness::builder()
    .register_integration(anthropic)
    .register_integration(codex)
    .register_tool(filesystem)
    .register_tool(shell)
    .register_extension(git_extension)
    .session_store(sqlite_store)
    .build()
    .await?;
```

Create a session:

```rust
let session = harness
    .session()
    .backend(backend)
    .workspace(workspace)
    .tools(agent_toolset)
    .budget(agent_budget)
    .start()
    .await?;
```

Interact:

```rust
let snapshot = session.snapshot().await?;

let mut events = session.subscribe();

session
    .send(SessionCommand::Prompt(
        UserInput::text("Fix the failing parser tests")
    ))
    .await?;
```

The frontend should not need direct access to `AgentRunner`, scheduler internals, or backend-specific types after construction.

---

# 64. Runtime Capability Discovery

Different builds expose different features.

Provide capability discovery.

```rust
pub struct RuntimeCapabilities {
    pub integrations: Vec<IntegrationDescriptor>,
    pub tools: Vec<ToolDescriptor>,
    pub extensions: Vec<ExtensionDescriptor>,
}
```

A UI can dynamically render available models/tools rather than hardcoding a build configuration.

---

# 65. Crate Layout

Suggested workspace:

```text
harness/
│
├── Cargo.toml
│
├── crates/
│   │
│   ├── harness-protocol/
│   │   ├── ids.rs
│   │   ├── messages.rs
│   │   ├── events.rs
│   │   ├── commands.rs
│   │   ├── tools.rs
│   │   ├── usage.rs
│   │   └── backend.rs
│   │
│   ├── harness-core/
│   │   ├── agent.rs
│   │   ├── agent_state.rs
│   │   ├── effects.rs
│   │   ├── transcript.rs
│   │   ├── capabilities.rs
│   │   └── budget.rs
│   │
│   ├── harness-runtime/
│   │   ├── runtime.rs
│   │   ├── session_manager.rs
│   │   ├── agent_supervisor.rs
│   │   ├── agent_runner.rs
│   │   ├── scheduler.rs
│   │   ├── cancellation.rs
│   │   ├── resource_manager.rs
│   │   └── permissions.rs
│   │
│   ├── harness-engine/
│   │   ├── harness.rs
│   │   ├── builder.rs
│   │   ├── session_builder.rs
│   │   └── handles.rs
│   │
│   ├── harness-model/
│   │   ├── client.rs
│   │   ├── request.rs
│   │   └── events.rs
│   │
│   ├── harness-generic-backend/
│   │   ├── backend.rs
│   │   └── loop.rs
│   │
│   ├── harness-tools/
│   │   ├── registry.rs
│   │   ├── executor.rs
│   │   └── policy.rs
│   │
│   ├── harness-context/
│   │   ├── engine.rs
│   │   ├── provider.rs
│   │   ├── budget.rs
│   │   └── compaction.rs
│   │
│   ├── harness-workspace/
│   │   ├── workspace.rs
│   │   ├── filesystem.rs
│   │   └── worktree.rs
│   │
│   ├── harness-session-store/
│   │   ├── store.rs
│   │   ├── sqlite.rs
│   │   └── jsonl.rs
│   │
│   ├── harness-extension-api/
│   │   ├── manifest.rs
│   │   ├── registry.rs
│   │   ├── observers.rs
│   │   └── interceptors.rs
│   │
│   ├── integrations/
│   │   ├── anthropic/
│   │   ├── openai/
│   │   ├── gemini/
│   │   ├── openai-compatible/
│   │   ├── claude-code/
│   │   └── codex/
│   │
│   ├── tools/
│   │   ├── filesystem/
│   │   ├── shell/
│   │   └── git/
│   │
│   └── transports/
│       ├── websocket/
│       ├── stdio/
│       └── ipc/
│
└── apps/
    ├── harness/
    │   └── standalone TUI
    │
    ├── harnessd/
    │   └── headless daemon
    │
    └── harnessctl/
        └── control/scripting CLI
```

This is a target shape, not a requirement to create every crate immediately.

---

# 66. Dependency Direction

Dependency direction matters more than exact crate names.

Desired:

```text
                   applications
                       │
                  harness-engine
                       │
                  harness-runtime
        ┌──────────────┼──────────────┐
        │              │              │
 integrations        tools          stores
        │              │              │
        └──────────────┼──────────────┘
                       │
                  harness-core
                       │
                harness-protocol
```

Avoid:

```text
core → TUI
core → Tauri
core → reqwest
core → Anthropic
core → filesystem implementation
core → SQLite
```

---

# 67. Cargo Features

Cargo features may be used for packaging, but must not become the architectural extension mechanism.

Prefer independent crates such as:

```text
harness-integration-anthropic
harness-integration-codex
harness-tool-shell
harness-transport-websocket
```

An application chooses what to link.

This enables different builds:

### Minimal/local

```text
local backend
filesystem
shell
git
TUI
```

### Full desktop

```text
Anthropic
OpenAI
Claude Code
Codex
MCP
IDE tools
persistence
```

### Headless daemon

```text
runtime
integrations
tools
RPC
no UI
```

---

# 68. Testing Strategy

The architecture should make the core highly testable.

## 68.1 Core transition tests

```rust
#[test]
fn tool_call_emits_execute_effect() {
    // build agent
    // feed normalized backend event
    // assert emitted effect
}
```

No network, Tokio runtime, or real tool required.

## 68.2 Fake backend

```rust
pub struct FakeBackend {
    scripted_events: Vec<ExecutionEvent>,
}
```

Use for full session tests.

## 68.3 Fake tools

Tools should support deterministic scripted results.

## 68.4 Replay tests

Persist command/event fixtures from real failures and replay them.

Goal:

```text
same initial state
+ same commands
= same semantic transitions
```

## 68.5 Concurrency tests

Test:

- two sessions streaming concurrently,
- one session cancellation does not cancel another,
- child failure isolation,
- scheduler limits,
- tool permission races,
- workspace conflicts.

## 68.6 Backend contract tests

Every backend implementation should pass a common conformance suite:

- streaming ordering,
- cancellation,
- completion,
- usage behavior,
- tool event normalization,
- error normalization.

---

# 69. Error Model

Use structured errors.

Potential domains:

```rust
pub enum HarnessError {
    Session(SessionError),
    Agent(AgentError),
    Backend(ExecutionError),
    Tool(ToolError),
    Workspace(WorkspaceError),
    Store(StoreError),
    Extension(ExtensionError),
}
```

Backend-specific errors should be normalized while preserving safe diagnostic metadata.

Avoid passing raw provider error blobs directly to the frontend.

---

# 70. Logging and Telemetry

Logging is separate from user-facing events.

Use structured tracing for internal diagnostics.

Useful dimensions:

```text
session_id
agent_id
run_id
request_id
tool_call_id
integration
model
```

Application-visible events should remain stable even if internal logging becomes more detailed.

---

# 71. Initial Implementation Sequence

Do not build the entire final architecture at once.

## Phase 1 — Core vertical slice

Implement:

1. IDs/protocol types.
2. `Agent`.
3. messages/transcript.
4. normalized execution events.
5. agent commands/effects.
6. usage ledger.
7. tool descriptor/capability types.

Goal:

```text
fake backend event
   ↓
Agent
   ↓
state + effects
```

No real provider required.

---

## Phase 2 — Single-session runtime

Implement:

1. `AgentRunner`.
2. `SessionRuntime`.
3. command channel.
4. event stream.
5. cancellation.
6. fake backend.
7. fake tool registry.

Goal:

```text
session.send(prompt)
session.subscribe()
```

works end-to-end.

---

## Phase 3 — Generic model backend

Implement:

1. `ModelClient`.
2. `GenericModelBackend`.
3. one real model provider.
4. streaming.
5. tool calls.
6. usage normalization.

Choose one API provider first.

Do not implement every provider.

---

## Phase 4 — Real tools

Implement:

```text
fs.read
workspace.search
fs.edit
shell.exec
```

Include:

- capability checks,
- permission policy,
- cancellation,
- tool events.

---

## Phase 5 — Multiple sessions

Implement:

1. `SessionManager`.
2. independent session tasks.
3. session-level event bus.
4. global scheduler.
5. independent injected backends.

Required test:

```text
Session A → Provider A
Session B → Provider B

both stream concurrently
```

---

## Phase 6 — Subagents

Implement:

1. `AgentSupervisor`.
2. child creation.
3. backend inheritance.
4. tool inheritance.
5. budget inheritance.
6. cancellation hierarchy.
7. child completion/failure events.

Required test:

```text
root spawns two children
children execute concurrently
root receives results
```

---

## Phase 7 — Persistence

Implement:

1. durable events,
2. SQLite store,
3. session restore,
4. transcript validation,
5. snapshot + event restoration.

---

## Phase 8 — Standalone TUI

Build a small terminal shell using only the public `Harness` / `SessionHandle` APIs.

This is an architectural validation step.

The TUI must not depend on runtime internals.

---

## Phase 9 — IDE integration

Replace or adapt the existing Pi sidecar path.

Two supported modes can coexist:

### Current-style sidecar

```text
FE → WebSocket → harnessd/sidecar
```

### Native/embedded

```text
IDE → Harness Engine
```

Both must use the same session semantics and event schema.

---

## Phase 10 — Specialized integrations

Add:

```text
Claude Code
Codex
```

behind `ExecutionBackend`.

This phase validates that the abstraction is high-level enough for execution systems that are not simple raw model APIs.

---

## Phase 11 — Extension SDK

Stabilize:

- tool registration,
- integration factories,
- context providers,
- event observers,
- interceptors.

Then consider:

- subprocess plugins,
- MCP adapter,
- WASM.

---

# 72. Architectural Invariants

These invariants should be enforced in code review.

## Core independence

The core must not know about:

```text
Tauri
Ratatui
WebSocket
Anthropic
OpenAI
Claude Code
Codex
SQLite
reqwest
filesystem implementation
```

## Per-session backend

Every session may have a different backend.

No global active provider.

## Per-agent capabilities

Tool visibility is defined by the agent.

Runtime registration does not imply agent access.

## Child non-escalation

A subagent cannot silently acquire non-delegatable parent capabilities.

## Observable execution

Meaningful execution stages emit normalized events.

## Transport independence

Changing WebSocket to direct Rust calls must not change session semantics.

## Failure isolation

One session failure cannot terminate unrelated sessions.

## Deterministic core

Core semantics should not perform arbitrary I/O or spawn tasks.

## Provider normalization

Provider-specific request/event shapes terminate at the backend boundary.

## Usage provenance

Usage is recorded per execution and aggregated upward.

## Unknown is not zero

Unavailable usage/cost data remains explicitly unknown.

---

# 73. Example End-to-End Flow

User opens two IDE chats.

```text
Chat A → Session A → Anthropic backend
Chat B → Session B → Codex backend
```

Session A receives:

```text
"Investigate the parser bug."
```

Flow:

```text
Frontend
  │
  │ SessionCommand::Prompt
  ▼
Session A
  │
  ▼
Root Agent A
  │
  │ AgentEffect::ExecuteBackend
  ▼
AnthropicBackend
  │
  ├── TextDelta
  ├── ToolCall(fs.read)
  └── Usage
       │
       ▼
AgentRunner
  │
  ├── updates agent
  ├── emits events
  └── executes permitted tool
       │
       ▼
SessionEventBus
       │
       ▼
IDE
```

Simultaneously:

```text
Session B
   ↓
CodexBackend
   ↓
tool execution
   ↓
Session B events
```

Neither blocks the other.

If Agent A spawns a child:

```text
Agent A
   │
   └── SpawnAgentSpec
         backend: Inherit
         tools: [fs.read, workspace.search]
         workspace: ReadOnly
         budget: 30k tokens
```

Runtime creates Agent A1.

IDE sees:

```text
AgentAdded A1
A1 status: WaitingForBackend
A1 tool: workspace.search
A1 text: ...
A1 completed
A usage updated
```

The root agent then receives the child result and continues.

---

# 74. Example Frontend Rendering

A rich IDE can render:

```text
Main Agent                                   82k context
● Working                                    $1.21

  Thinking
  Investigating the parser and precedence handling...

  Tool
  read_file src/parser.rs

  Tool
  cargo test parser

  Subagents
  ├─ Research Agent      ● searching
  └─ Test Agent          ✓ completed

  Assistant
  I found the issue in...
```

A terminal application can render the same events more simply:

```text
● reading src/parser.rs
● running cargo test
● 2 subagents
> I found the issue...
```

Same engine. Different presentation.

---

# 75. Development Decision Checklist

Before adding a feature, ask:

### Does this belong in core?

Only if it defines agent/session semantics or a stable interface.

### Does this perform I/O?

It probably belongs in runtime or an integration.

### Does this depend on a frontend?

It belongs outside the engine.

### Does this apply to every provider?

If not, keep it behind backend capability or integration code.

### Is this a tool registration or an agent grant?

Registration belongs to runtime. Grant belongs to agent capabilities.

### Is this an event or current state?

Events describe changes. Snapshots describe current truth.

### Can a child inherit this safely?

Make inheritance and delegation explicit.

### Can a backend fail to expose this data?

Represent it as optional rather than inventing a value.

### Can the same code work embedded and remote?

If not, transport concerns may be leaking inward.

---

# 76. Recommended First Public Interfaces

The initial stable surface should be intentionally small.

## Harness

```rust
pub trait HarnessApi {
    async fn create_session(
        &self,
        spec: SessionSpec,
    ) -> Result<SessionHandle>;

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<SessionHandle>;

    fn capabilities(&self) -> RuntimeCapabilities;
}
```

## Session

```rust
pub trait SessionApi {
    fn id(&self) -> SessionId;

    async fn send(
        &self,
        command: SessionCommand,
    ) -> Result<()>;

    async fn snapshot(
        &self,
    ) -> Result<SessionSnapshot>;

    fn subscribe(
        &self,
    ) -> SessionEventStream;
}
```

## Execution backend

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;
    fn capabilities(&self) -> BackendCapabilities;

    async fn execute(
        &self,
        request: ExecutionRequest,
        context: ExecutionContext,
        events: ExecutionEventSink,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError>;
}
```

## Tool executor

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;

    async fn execute(
        &self,
        context: ToolExecutionContext,
        arguments: serde_json::Value,
        events: ToolEventSink,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}
```

## Workspace

```rust
#[async_trait]
pub trait Workspace: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn search(&self, query: SearchQuery) -> Result<SearchResult>;
}
```

These contracts provide enough flexibility to build the rest without prematurely stabilizing every internal detail.

---

# 77. Final Architectural Principle

The target system is not merely a replacement for a single coding-agent harness.

It is a **portable agent execution platform**.

The architecture should make all of these equivalent hosts:

```text
               Harness Engine
                     │
        same Session / Agent semantics
                     │
       ┌─────────────┼─────────────┐
       ▼             ▼             ▼
      IDE           TUI          Daemon
       │             │             │
 direct embed    local shell      RPC
```

And all of these valid execution bindings:

```text
Session A → Anthropic
Session B → OpenAI
Session C → Claude Code
Session D → Codex
Session E → custom backend
```

And all of these valid agent topologies:

```text
single agent

root
 ├── researcher
 └── implementer

root
 ├── Gemini researcher
 ├── Codex coder
 └── Claude reviewer
```

The core remains unchanged.

That is the architectural property to protect throughout implementation.

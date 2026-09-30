# Harness Orchestration Layer — Architecture and Minimal Default Design

**Status:** Implemented (first orchestration layer, §21) — see §25  
**Scope:** Harness/runtime architecture only; no visual designer or UI implementation  
**Primary goal:** Introduce a simple, deterministic orchestration layer above the current agent execution loop, while preserving the current harness as the execution substrate and making future visual orchestration possible.

---

## 1. Executive summary

Rusty already has a strong **agent execution layer**:

- a deterministic `Agent::apply(command) -> effects` state machine;
- asynchronous effect execution in `AgentRunner`;
- normalized model backend and tool registry abstractions;
- tool permissions and user approval;
- child-agent supervision;
- cancellation, pause, resume, steering, and follow-up commands;
- durable events, projections, checkpoints, and restoration;
- structured model output through `ResponseFormat::JsonSchema`.

What it does not yet have is a first-class layer that controls a **multi-step task workflow** above an individual model/tool loop.

The proposed orchestration layer fills that gap:

```text
User request
    ↓
Orchestration definition
    ↓
Orchestration runtime
    ├── selects ready step(s)
    ├── prepares step input/context
    ├── configures an agent run
    ├── delegates execution to SessionRuntime
    ├── observes durable agent events
    ├── validates step output
    └── applies deterministic transition rules
    ↓
Final output validation
    ↓
Orchestration result
```

The important boundary is:

> The orchestrator controls workflow progression. The existing agent runtime controls model turns, tool calls, permissions, and agent-local execution.

The first implementation should be deliberately small. It should support a default sequential workflow with these logical stages:

```text
Receive input
  → Execute agent step
  → Verify result
  → Format final output
  → Complete or fail
```

The persisted definition must be declarative and graph-compatible from the start, even though the first runtime may execute only a restricted subset. This allows a future canvas editor to author the same document without changing runtime semantics.

---

## 2. Problem statement

Today, the model implicitly determines much of the flow inside an agent run:

```text
prepare context
  → request model
  → model requests tools or returns text
  → execute tools
  → request model again
  → finish
```

That loop is useful but insufficient for product-level orchestration because it does not explicitly represent:

- named workflow steps;
- legal transitions between steps;
- step-specific models, prompts, tools, and output schemas;
- retries and failure routes;
- acceptance criteria;
- intermediate outputs and evidence;
- workflow-wide budgets;
- deterministic completion rules;
- resumable workflow state;
- a stable graph that can later be visually edited.

Optimizing tools improves individual actions. Orchestration controls **why an action is available, when it may run, what must be produced, and what happens next**.

---

## 3. Goals

### 3.1 Functional goals

The orchestration layer should:

1. Represent a workflow as a versioned declarative definition.
2. Validate the definition before execution.
3. Maintain explicit, durable workflow-run state.
4. Select legal next steps deterministically.
5. Delegate semantic work to the existing agent/session runtime.
6. Restrict tools and execution parameters per step.
7. support structured outputs using JSON Schema.
8. Validate step output before allowing progression.
9. Support bounded retry and failure handling.
10. Produce a final result conforming to a selected output schema.
11. Emit structured events suitable for tracing, replay, and future UI visualization.
12. Pause safely for existing permission requests or future explicit human-input steps.
13. Remain compatible with current direct, non-orchestrated sessions.

### 3.2 Architectural goals

The design should:

- preserve `harness-core` as deterministic domain logic without I/O;
- keep provider-specific behavior behind `ExecutionBackend`;
- keep tool execution behind `ToolRegistry`;
- reuse `SessionRuntime` rather than duplicating the agent loop;
- use commands and events rather than direct mutation across boundaries;
- make orchestration definitions serializable and provider-neutral;
- separate immutable definitions from mutable run state;
- separate semantic model decisions from operational control;
- support durable recovery without repeating completed side effects;
- be authorable by code first and by a canvas later.

### 3.3 Non-goals for the first version

The first version should not attempt to provide:

- a visual graph editor;
- arbitrary scripting inside graph nodes;
- unrestricted cyclic graphs;
- distributed orchestration across multiple processes;
- speculative execution or tree search;
- automatic workflow generation by a model;
- a marketplace for workflow node plug-ins;
- a full BPMN-compatible workflow engine;
- general-purpose event processing;
- replacement of the current `Agent` state machine;
- exposure of private model reasoning.

---

## 4. Relationship to the current harness

### 4.1 Existing execution responsibilities

The current harness already has three important layers.

#### Deterministic agent domain (`harness-core`)

The agent receives `AgentCommand` values and returns `AgentEffect` values. It owns agent-local state such as:

- status and current operation;
- canonical message history;
- context bookkeeping;
- active run;
- queued input;
- pending tools and permission correlations;
- child agents;
- execution parameters;
- last error and deterministic transition sequence.

This should remain the authority for a single agent's state transitions.

#### Runtime effects (`harness-runtime`)

`AgentRunner` executes effects by:

- calling an `ExecutionBackend`;
- executing registered tools;
- requesting permission;
- spawning or cancelling child agents;
- publishing events;
- persisting mutations;
- handling cancellation tokens;
- reporting a final agent result.

`SessionRuntime` owns the session, root agent, event bus, scheduler, integrations, live projection, durable committer, and checkpointing.

#### Protocol (`harness-protocol`)

The protocol defines stable commands and events, including:

- `StartRun`, `Steer`, `FollowUp`, `Pause`, `Resume`, and `Cancel`;
- backend events and execution parameters;
- tool completion and failure;
- permission requests and resolutions;
- child-agent lifecycle;
- agent completion and failure;
- JSON and JSON Schema response formats.

### 4.2 New orchestration responsibility

The orchestrator should sit above `SessionRuntime`:

```text
OrchestrationRuntime
    ↓ sends commands / observes events
SessionRuntime
    ↓ owns
AgentRunner
    ↓ applies
Agent state machine
    ↓ produces
Backend, tool, permission, child-agent effects
```

It should not intercept model tokens and reproduce the existing tool loop. It should treat one orchestrated agent step as a bounded unit of delegated work.

### 4.3 What remains unchanged

A direct session remains valid:

```text
client → SessionRuntime → root AgentRunner
```

An orchestrated session adds an optional controller:

```text
client → OrchestrationRuntime → SessionRuntime → root AgentRunner
```

This makes orchestration additive and backward-compatible.

---

## 5. Core design principles

### 5.1 Definitions are immutable; runs are mutable

An `OrchestrationDefinition` describes what may happen. An `OrchestrationRun` records what did happen.

Definitions should never be mutated while a run is active. Editing a workflow creates a new definition revision.

### 5.2 The model proposes semantics; code controls operations

The model may produce classifications, plans, analyses, or structured results. Deterministic code controls:

- which step runs;
- which tools are available;
- legal graph transitions;
- retries and limits;
- schema validation;
- approval gates;
- persistence;
- completion.

### 5.3 Every transition has a reason and evidence

A step should not become `Succeeded` merely because an assistant emitted text. It succeeds after the configured output contract and completion policy pass.

### 5.4 Persist before advancing

A completed step's output and terminal state must be durably recorded before downstream steps become runnable. This avoids repeating side effects after recovery.

### 5.5 Restricted graph first

The data model may represent a graph, but the first executor should support a safe subset:

- one start node;
- one terminal output node;
- directed acyclic control flow;
- one active agent step at a time;
- explicit success and failure transitions;
- bounded retries;
- no arbitrary loops.

This prevents the first version from becoming a general workflow engine before its semantics are stable.

### 5.6 Output schemas are contracts, not prompt decoration

A selected schema affects:

1. the backend request through `ExecutionParams.response_format` when supported;
2. runtime-side parsing and validation regardless of provider support;
3. the data passed to subsequent steps;
4. the final public result contract.

Provider enforcement is helpful, but host validation remains authoritative.

---

## 6. Conceptual model

The orchestration domain has six primary objects:

```text
OrchestrationDefinition  immutable workflow template
OrchestrationNode        one typed unit of work/control
OrchestrationEdge        one legal transition/dependency
OrchestrationRun         one execution of a definition revision
StepRun                  one node's execution state and attempts
Artifact                 typed output/evidence produced during a run
```

### 6.1 Definition versus run

```text
Definition                         Run
----------                         ---
id                                 run_id
revision                           definition_id + revision
nodes                              status
edges                              step_runs
input_schema                       input
output_schema                      artifacts
policies                           budgets consumed
                                   active/ready steps
                                   final output or error
```

### 6.2 Control flow versus data flow

The graph should distinguish:

- **Control edges:** determine what may execute next.
- **Data bindings:** determine what data a node receives.

A line on a future canvas may visualize both, but runtime semantics should not infer data transfer merely from visual proximity or edge order.

---

## 7. Orchestration components

## 7.1 Definition registry

### Responsibility

Stores and resolves immutable orchestration definitions by ID and revision.

### Controls

- definition identity;
- revision selection;
- publication status;
- compatibility metadata;
- built-in default definition.

### State

```rust
pub enum DefinitionStatus {
    Draft,
    Published,
    Deprecated,
}
```

### Rules

- A run references an exact revision.
- Published revisions are immutable.
- A draft cannot execute unless explicitly allowed in development mode.
- Removing a definition must not invalidate historical runs.

---

## 7.2 Definition validator/compiler

### Responsibility

Validates a declarative graph and compiles it into an execution-friendly representation.

### Controls

- node and edge validity;
- entry and terminal node requirements;
- reachability;
- cycle restrictions;
- unique IDs;
- port compatibility;
- schema validity;
- tool references;
- capability requirements;
- transition determinism;
- unsupported first-version features.

### Output

```rust
pub struct CompiledOrchestration {
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub entry_node: OrchestrationNodeId,
    pub nodes: HashMap<OrchestrationNodeId, CompiledNode>,
    pub outgoing: HashMap<OrchestrationNodeId, Vec<CompiledEdge>>,
    pub incoming: HashMap<OrchestrationNodeId, Vec<CompiledEdge>>,
    pub input_schema: Option<JsonSchema>,
    pub output_schema: OutputSchema,
}
```

### Compile-time validation examples

- exactly one `Input` node;
- at least one reachable `Output` node;
- every non-terminal node has a legal outgoing route;
- no dangling edges;
- a condition may not reference unavailable data;
- retry counts are bounded;
- referenced tools exist or are declared as deployment requirements;
- a `JsonSchema` output is syntactically valid;
- unsupported cycles are rejected before execution.

The compiler should return all discoverable issues in one report rather than failing on the first issue.

---

## 7.3 Orchestration runtime

### Responsibility

Owns one orchestration run and advances it through deterministic transitions.

### Controls

- run lifecycle;
- ready-step selection;
- step admission;
- dispatch to a step executor;
- application of step outcomes;
- pause/resume/cancel behavior;
- workflow-level completion;
- durable checkpoint boundaries.

### Does not control

- provider wire formats;
- token streaming details;
- implementation of tools;
- agent-internal tool loops;
- provider-native reasoning.

### Main loop

```text
load compiled definition and run state
  → validate/admit input
  → calculate ready steps
  → dispatch permitted step
  → observe step outcome
  → validate and persist outcome
  → apply transition
  → repeat until terminal or paused
```

For the first version, dispatch one step at a time. A future scheduler can execute independent ready steps concurrently without changing the definition format.

---

## 7.4 Run-state reducer

### Responsibility

Implements deterministic state transitions without performing I/O.

A suitable conceptual API is:

```rust
pub fn apply(
    state: &mut OrchestrationRunState,
    command: OrchestrationCommand,
) -> Vec<OrchestrationEffect>;
```

This mirrors the existing `Agent::apply` architecture and gives orchestration the same testability and replay properties.

### Controls

- whether a command is valid in the current state;
- step-status transitions;
- attempt counters;
- ready-node calculation;
- retry admission;
- workflow terminal outcome;
- emitted orchestration events and requested effects.

### Benefit

The reducer can be exhaustively unit tested without a model, filesystem, network, or Tokio runtime.

---

## 7.5 Step scheduler

### Responsibility

Chooses which eligible step to run next.

### First-version policy

- deterministic ordering by compiled topological rank, then node ID;
- maximum concurrency of one;
- do not dispatch while run is paused, cancelling, or waiting for input;
- do not dispatch a downstream node until all required dependencies succeeded;
- never dispatch more than the remaining workflow budget permits.

### Future policy

- parallel independent branches;
- resource pools;
- provider-specific rate limits;
- priority classes;
- maximum parallel model/tool requests;
- fairness across runs.

Scheduling should remain runtime code, not a model decision.

---

## 7.6 Step executor

### Responsibility

Executes one typed node and returns a normalized `StepOutcome`.

```rust
pub enum StepOutcome {
    Succeeded {
        output: serde_json::Value,
        artifacts: Vec<ArtifactRef>,
        evidence: Vec<EvidenceRef>,
    },
    Failed {
        error: OrchestrationError,
        retryable: bool,
    },
    WaitingForInput {
        request: InputRequest,
    },
    Cancelled,
}
```

The executor dispatches by node kind. It should not choose the next node; that belongs to the reducer and transition evaluator.

---

## 7.7 Agent-step adapter

### Responsibility

Bridges an orchestration `Agent` node to the existing `SessionRuntime`.

### Inputs

- rendered step instructions;
- selected prior outputs and artifacts;
- step execution parameters;
- allowed tool policy;
- expected output schema;
- timeout and attempt metadata.

### Actions

1. Build a bounded `UserInput` for the step.
2. Apply step-level `ExecutionParams` using the existing configuration command.
3. Restrict visible tools through a scoped registry/view.
4. Start the agent run.
5. Correlate events by orchestration run, step, attempt, session, and agent run IDs.
6. Wait for completion, cancellation, or failure.
7. collect the assistant result or structured artifact.
8. Return a normalized `StepOutcome`.

### Important constraint

The existing root agent is long-lived and retains conversation history. For strict step isolation, the first orchestration implementation should prefer one of these explicit modes:

```rust
pub enum AgentContextMode {
    SharedSession,
    IsolatedChild,
}
```

Recommended default: `IsolatedChild` for workflow steps. It prevents unrelated step history from contaminating subsequent steps and makes retry semantics clearer. `SharedSession` can remain available for conversational workflows.

---

## 7.8 Context and input assembler

### Responsibility

Builds the exact input view for a step from declared bindings.

### Controls

- original orchestration input;
- outputs from predecessor nodes;
- selected artifacts;
- static instructions;
- run metadata;
- maximum context size;
- untrusted-content boundaries.

### Binding examples

```text
$.input.request
$.steps.research.output.summary
$.steps.implementation.artifacts
$.run.metadata.workspace_root
```

The first version should use a small declarative binding language rather than arbitrary code. JSON Pointer or a restricted JSONPath subset is sufficient.

### Security rule

Retrieved files, tool output, and upstream model output are data. The assembler must keep them distinguishable from trusted system and developer instructions.

---

## 7.9 Tool-scope resolver

### Responsibility

Provides the step with a filtered view of the existing `ToolRegistry`.

### Policy

```rust
pub enum ToolScope {
    None,
    AllowList(Vec<ToolId>),
    Inherit,
}
```

The first version should avoid deny lists because newly registered tools could become available unintentionally. Explicit allow lists provide safer, reviewable behavior.

### Controls

- tools described to the model;
- tools executable by the runtime;
- optional per-tool limits;
- whether a tool always requires approval;
- whether a tool is unavailable for retries.

Visibility and execution authorization must use the same resolved scope. A hidden tool must not remain executable by name.

---

## 7.10 Policy engine

### Responsibility

Evaluates operational policies before effects are executed.

### Policy categories

- tool authorization;
- workspace/path boundaries;
- human approval requirements;
- model/provider restrictions;
- retry eligibility;
- timeout limits;
- cost/token/tool-call budgets;
- child-agent limits;
- side-effect classification.

### Decision

```rust
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
}
```

The current harness already supports tool permission requests. The orchestration policy engine should compose with that mechanism rather than invent a second user-approval channel for tool calls.

---

## 7.11 Schema registry and validator

### Responsibility

Stores reusable schema definitions and validates input, intermediate, and final output.

### Schema roles

- orchestration input schema;
- per-node input schema;
- per-node output schema;
- final output schema;
- reusable named schemas.

### Output schema selection

The definition owns the final contract:

```rust
pub struct OutputContract {
    pub schema: SchemaReference,
    pub source: OutputBinding,
    pub strict: bool,
}
```

`SchemaReference` may be inline or named:

```rust
pub enum SchemaReference {
    Inline {
        name: String,
        schema: serde_json::Value,
    },
    Registry {
        schema_id: String,
        revision: u64,
    },
}
```

### Validation sequence

```text
model/provider structured-output enforcement, when available
  → parse JSON
  → host-side JSON Schema validation
  → optional semantic verification
  → persist validated value
```

A provider lacking `structured_output` must not silently bypass the contract. The runtime may either reject the workflow based on capability policy or request JSON text and validate it host-side, depending on the node configuration.

---

## 7.12 Artifact and evidence store

### Responsibility

Stores outputs too large or unsuitable for inline run state.

### Artifact examples

- generated files;
- reports;
- patches;
- structured JSON output;
- test results;
- citations;
- tool result snapshots.

### Evidence examples

- test command succeeded;
- output matched schema;
- source excerpt supports a claim;
- required file exists;
- approval was granted.

An artifact is a produced object. Evidence is a claim about why a criterion is satisfied. One object can serve as both through separate references.

---

## 7.13 Budget manager

### Responsibility

Tracks and enforces limits at orchestration and step scopes.

```rust
pub struct OrchestrationBudget {
    pub max_elapsed_ms: Option<u64>,
    pub max_model_requests: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub max_step_attempts: u32,
}
```

### Rules

- A step budget cannot exceed the remaining run budget.
- Unknown cost must remain unknown; it must not be treated as zero.
- A budget denial is deterministic and produces a machine-readable failure.
- Retry consumes the same global budget and a new attempt budget.

---

## 7.14 Transition evaluator

### Responsibility

Selects the legal outgoing edge after a node reaches a terminal attempt outcome.

### First-version edge conditions

```rust
pub enum EdgeCondition {
    OnSuccess,
    OnFailure,
    Always,
    Expression(RestrictedExpression),
}
```

The MVP default workflow only needs `OnSuccess` and `OnFailure`. A restricted expression language may follow after its type and security semantics are defined.

### Determinism rules

- at most one matching control edge in the MVP;
- ambiguous matches fail validation or execution;
- conditions inspect only persisted normalized state;
- condition evaluation performs no I/O;
- edge order is not semantic.

---

## 7.15 Event recorder and checkpoint manager

### Responsibility

Persists orchestration events and snapshots with ordering and recovery guarantees comparable to current session durability.

### Controls

- run event sequence;
- append-before-publish semantics;
- snapshot cadence;
- restoration point;
- correlation to session and agent events;
- redaction of sensitive values.

### Correlation fields

Every step-attributable event should carry:

```text
orchestration_run_id
orchestration_definition_id
orchestration_revision
node_id
step_run_id
attempt
session_id (when delegated)
agent_id (when delegated)
agent_run_id (when delegated)
```

This makes future canvas visualization and debugging possible without parsing messages.

---

## 8. Node/component catalog

The long-term definition can support several node kinds, but only a subset should execute in the first implementation.

## 8.1 `Input` node

### Purpose

Admits and validates the request that starts the workflow.

### Configuration

- input JSON Schema;
- optional default values;
- attachment policy;
- normalization rules.

### Inputs

External request and attachments.

### Outputs

Validated normalized input.

### States

`Pending → Running → Succeeded | Failed`

### Controls

Whether the workflow is admitted. It performs no model call.

### MVP

Required and supported.

---

## 8.2 `Agent` node

### Purpose

Delegates semantic work to an existing harness agent.

### Configuration

- role/name;
- instruction template;
- input bindings;
- model/execution parameter overrides;
- tool scope;
- context mode;
- output schema;
- timeout;
- retry policy;
- completion policy.

### Inputs

Bound orchestration input, predecessor outputs, and artifacts.

### Outputs

Validated structured value, optional text, artifacts, and evidence.

### States

Full step lifecycle, including waiting for permission.

### Controls

What the model is asked to do and which capabilities it can access. It does not control graph progression directly.

### MVP

Required and supported.

---

## 8.3 `Verify` node

### Purpose

Checks whether prior work satisfies explicit criteria.

### Modes

```rust
pub enum VerificationMode {
    Deterministic,
    AgentAssisted,
    Composite,
}
```

### Configuration

- assertions/checks;
- evidence bindings;
- optional verifier instructions;
- output schema;
- pass threshold;
- retry target policy.

### Outputs

```json
{
  "passed": true,
  "checks": [
    { "id": "schema", "passed": true, "evidence": ["artifact:result"] }
  ],
  "issues": []
}
```

### Controls

Whether downstream completion is allowed. It does not rewrite the producer's output.

### MVP

Support a deterministic schema-validation verifier first. Agent-assisted verification may follow.

---

## 8.4 `Transform` node

### Purpose

Maps existing structured data into a new shape without a model.

### Configuration

- input bindings;
- restricted mapping expression;
- output schema.

### Controls

Deterministic data shaping only; no tools or side effects.

### MVP

Optional. The first default workflow can use an output binding instead.

---

## 8.5 `Condition` node

### Purpose

Chooses a branch based on persisted structured state.

### Configuration

- restricted boolean expressions;
- named output routes;
- fallback route.

### Controls

Branch selection only.

### MVP

Do not expose as a standalone node initially. Use success/failure edges.

---

## 8.6 `Approval` node

### Purpose

Pauses the workflow for an explicit human decision unrelated to a single tool's permission request.

### Configuration

- prompt;
- data shown to reviewer;
- allowed decisions;
- timeout/escalation policy.

### States

`Pending → WaitingForInput → Succeeded | Failed | Cancelled`

### Controls

Release of a workflow transition.

### MVP

Deferred. Existing tool permission requests remain supported through delegated agent execution.

---

## 8.7 `Parallel` / `Join` nodes

### Purpose

Express fan-out and synchronization.

### Controls

- branch activation;
- join policy (`all`, `any`, threshold);
- cancellation of losing branches;
- aggregation.

### MVP

Deferred. The definition model should not preclude them, but the compiler must reject them until implemented.

---

## 8.8 `Subflow` node

### Purpose

Invokes another published orchestration definition.

### Controls

- child definition revision;
- input/output mapping;
- inherited budget;
- cancellation propagation.

### MVP

Deferred. Child agents are not equivalent to subflows: a child agent executes a semantic task, while a subflow executes another explicit workflow.

---

## 8.9 `Output` node

### Purpose

Selects, validates, and publishes the final orchestration result.

### Configuration

- source binding;
- selected output schema;
- strictness;
- optional presentation metadata.

### Inputs

Persisted outputs and artifacts from prior steps.

### Outputs

The public `OrchestrationResult`.

### Controls

The final contract and completion gate.

### MVP

Required and supported.

---

## 8.10 `Fail` node

### Purpose

Terminates the workflow with a normalized domain error.

### Configuration

- error code;
- message template;
- included diagnostic bindings;
- public/private detail policy.

### MVP

May be represented as a terminal failure transition rather than a visible node. The serialized model should allow an explicit node later.

---

## 9. State model

## 9.1 Orchestration run states

```rust
pub enum OrchestrationStatus {
    Created,
    ValidatingInput,
    Ready,
    Running,
    WaitingForPermission,
    WaitingForInput,
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}
```

### State meanings

| State | Meaning | Allowed external actions |
|---|---|---|
| `Created` | Run identity exists, no input admitted | start, cancel |
| `ValidatingInput` | Input contract is being checked | cancel |
| `Ready` | At least one step may be scheduled | pause, cancel |
| `Running` | A step is executing or runtime is advancing state | pause, cancel |
| `WaitingForPermission` | Delegated agent awaits a tool decision | resolve permission, cancel |
| `WaitingForInput` | Workflow node explicitly awaits user input | provide input, cancel |
| `Paused` | No new work may be admitted | resume, cancel |
| `Cancelling` | Cancellation is propagating | observe only |
| `Completed` | Final output validated and persisted | observe only |
| `Failed` | Unrecoverable terminal error persisted | observe/retry as new run |
| `Cancelled` | Cancellation completed | observe only |

A run must never return to a non-terminal state after `Completed`, `Failed`, or `Cancelled`.

## 9.2 Step states

```rust
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    WaitingForPermission,
    WaitingForInput,
    RetryScheduled,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}
```

### Legal primary transitions

```text
Pending → Ready
Ready → Running | Cancelled
Running → WaitingForPermission | WaitingForInput
Running → Succeeded | Failed | Cancelled
WaitingForPermission → Running | Failed | Cancelled
WaitingForInput → Running | Failed | Cancelled
Failed → RetryScheduled          only if policy and budget allow
RetryScheduled → Ready
Pending → Skipped                branch not selected
```

Terminal step states are `Succeeded`, `Skipped`, `Cancelled`, and non-retryable `Failed`.

## 9.3 Attempt state

Each retry creates a distinct immutable attempt record:

```rust
pub struct StepAttempt {
    pub attempt: u32,
    pub status: AttemptStatus,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub delegated_run: Option<DelegatedRunRef>,
    pub input_snapshot: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub artifacts: Vec<ArtifactRef>,
    pub evidence: Vec<EvidenceRef>,
    pub error: Option<OrchestrationError>,
    pub usage: UsageSummary,
}
```

Do not overwrite failed attempt data when retrying. It is essential for diagnosis and replay.

---

## 10. Commands, effects, and events

## 10.1 Commands

Commands request state transitions:

```rust
pub enum OrchestrationCommand {
    Start { input: serde_json::Value },
    StepAdmitted { node_id: OrchestrationNodeId, attempt: u32 },
    StepSucceeded { node_id: OrchestrationNodeId, attempt: u32, result: StepResult },
    StepFailed { node_id: OrchestrationNodeId, attempt: u32, error: OrchestrationError },
    PermissionRequired { node_id: OrchestrationNodeId, permission_id: PermissionId },
    PermissionResolved { permission_id: PermissionId, decision: PermissionDecision },
    InputRequired { node_id: OrchestrationNodeId, request: InputRequest },
    InputProvided { request_id: InputRequestId, value: serde_json::Value },
    Pause,
    Resume,
    Cancel,
    RetryStep { node_id: OrchestrationNodeId },
}
```

Internal completion commands must include node ID and attempt so stale events cannot complete a newer retry.

## 10.2 Effects

Effects request I/O or runtime work:

```rust
pub enum OrchestrationEffect {
    ExecuteStep { node_id: OrchestrationNodeId, attempt: u32 },
    CancelStep { node_id: OrchestrationNodeId, attempt: u32 },
    PersistEvent { event: OrchestrationEventEnvelope },
    SaveCheckpoint { snapshot: serde_json::Value },
    Emit { event: OrchestrationEvent },
    RequestInput { request: InputRequest },
    Finish { result: OrchestrationResult },
}
```

As in the agent runtime, deterministic transitions produce effects and an async runner interprets them.

## 10.3 Events

Minimum event catalog:

```text
RunCreated
InputValidated
RunStateChanged
StepReady
StepStarted
StepStateChanged
StepOutputProduced
StepValidationFailed
StepRetryScheduled
PermissionRequested
PermissionResolved
InputRequested
InputProvided
BudgetUpdated
ArtifactRecorded
TransitionSelected
RunCompleted
RunFailed
RunCancelled
```

Events should be structured; human-readable logs are a projection, not the source of truth.

---

## 11. Declarative definition format

The persisted format must be independent of React Flow. Visual coordinates, selection state, and measured dimensions belong to editor metadata, not runtime semantics.

```rust
pub struct OrchestrationDefinition {
    pub schema_version: u32,
    pub id: OrchestrationDefinitionId,
    pub revision: u64,
    pub name: String,
    pub description: Option<String>,
    pub status: DefinitionStatus,
    pub input_schema: Option<SchemaReference>,
    pub output_contract: OutputContract,
    pub nodes: Vec<OrchestrationNode>,
    pub edges: Vec<OrchestrationEdge>,
    pub policies: OrchestrationPolicies,
    pub metadata: serde_json::Value,
}
```

### 11.1 Node envelope

```rust
pub struct OrchestrationNode {
    pub id: OrchestrationNodeId,
    pub name: String,
    pub kind: OrchestrationNodeKind,
    pub input_bindings: Vec<InputBinding>,
    pub output_schema: Option<SchemaReference>,
    pub retry: RetryPolicy,
    pub timeout_ms: Option<u64>,
    pub metadata: serde_json::Value,
}
```

`metadata` may later contain editor layout, labels, colors, and grouping. Runtime code must ignore unknown presentation metadata.

### 11.2 Edge envelope

```rust
pub struct OrchestrationEdge {
    pub id: OrchestrationEdgeId,
    pub source: OrchestrationNodeId,
    pub target: OrchestrationNodeId,
    pub condition: EdgeCondition,
    pub priority: Option<u32>,
    pub metadata: serde_json::Value,
}
```

Priority should be unused in the MVP because ambiguous transitions should be rejected. It can support ordered fallback semantics later if deliberately specified.

### 11.3 Compatibility

- Include `schema_version` from the beginning.
- Unknown node kinds must fail closed.
- Unknown optional metadata must be preserved when possible.
- Definition migration must occur separately from run restoration.
- A run always records the exact definition revision and preferably a content hash.

---

## 12. Default orchestration

The first built-in orchestration should be useful but intentionally simple.

## 12.1 Logical graph

```text
[Input]
   |
   v
[Execute]
   | success
   v
[Verify]
   | pass
   v
[Output]

Execute failure ───────────────→ Fail
Verify failure + retry allowed → Execute
Verify failure otherwise ─────→ Fail
```

Because arbitrary cycles are deferred, the `Verify → Execute` retry should be represented internally as a bounded retry policy on the execute/verify stage, not as a free graph cycle in version one.

## 12.2 Component behavior

### Input

- Accept the user's original request and attachments.
- Normalize it into a stable object.
- Validate required content.

Example normalized input:

```json
{
  "request": "Add password reset support",
  "attachments": [],
  "workspace": {
    "root": "/workspace"
  }
}
```

### Execute

- Use an isolated agent context.
- Provide the original request as the goal.
- Use the current active model unless overridden.
- Inherit the harness's default tool registry through an explicit resolved allow list.
- Reuse current permission handling.
- Produce a structured execution report.

Suggested output schema:

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": ["summary", "status", "artifacts", "claimsToVerify"],
  "properties": {
    "summary": { "type": "string" },
    "status": { "enum": ["completed", "blocked", "failed"] },
    "artifacts": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "reference"],
        "properties": {
          "kind": { "type": "string" },
          "reference": { "type": "string" }
        }
      }
    },
    "claimsToVerify": {
      "type": "array",
      "items": { "type": "string" }
    }
  }
}
```

### Verify

For the minimal implementation:

- validate the execute output schema;
- verify required artifacts are resolvable;
- require `status == "completed"`;
- record checks and evidence;
- do not rely only on model-reported confidence.

Later, task-specific deterministic checks or an independent verifier agent can be added.

### Output

- Bind to the validated execution report.
- Apply the workflow's selected final output schema.
- Persist the final result.
- Emit `RunCompleted` only after persistence succeeds.

## 12.3 Default failure behavior

- Invalid input: fail without a model request.
- Backend transient error: retry according to step policy.
- Tool denial: return to the agent as today; fail only if the agent cannot continue.
- Schema-invalid model output: retry once with validation errors included as data.
- Verification failure: retry execute at most once by default.
- Budget exhausted: fail with `BUDGET_EXHAUSTED`.
- Cancellation: propagate to delegated session/agent and terminate as `Cancelled`.
- Persistence failure at the authoritative commit boundary: fail closed.

---

## 13. Retry semantics

```rust
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffPolicy,
    pub retry_on: Vec<RetryReason>,
}
```

Recommended reasons:

```rust
pub enum RetryReason {
    BackendRateLimited,
    BackendTimeout,
    ToolTimeout,
    InvalidStructuredOutput,
    VerificationFailed,
}
```

Do not retry automatically for:

- permission denial;
- invalid workflow definition;
- invalid input;
- unsupported provider capability unless a fallback is configured;
- deterministic policy denial;
- destructive side effects with unknown completion;
- budget exhaustion;
- cancellation.

A retry must be a new attempt with a new delegated run ID and preserved prior evidence.

---

## 14. Completion and final schema

An orchestration may complete only when all of these are true:

1. A terminal output node is reached.
2. Every required predecessor is in an accepted terminal state.
3. No required step is still running or awaiting input.
4. The output binding resolves successfully.
5. The resulting value validates against the selected final schema.
6. Required evidence policies pass.
7. The final result and completion event are durably committed.

The result contract should be provider-neutral:

```rust
pub struct OrchestrationResult {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub revision: u64,
    pub status: OrchestrationOutcome,
    pub output: Option<serde_json::Value>,
    pub artifacts: Vec<ArtifactRef>,
    pub usage: UsageSummary,
    pub error: Option<OrchestrationError>,
}
```

The final schema is selected at definition level, not guessed at runtime. A future UI can present a schema picker/editor that writes `output_contract`; the harness will consume the same field.

---

## 15. Persistence and restoration

The orchestration layer should follow the durability principles already present in the session runtime.

### 15.1 Persisted run data

- run identity and status;
- definition ID, revision, and content hash;
- normalized input;
- step states and immutable attempts;
- outputs, artifact references, and evidence references;
- consumed budget and usage;
- pending permission/input correlation;
- delegated session, agent, and run IDs;
- selected transitions;
- terminal result or error;
- monotonic orchestration event sequence.

### 15.2 Checkpoint moments

- after input admission;
- before dispatching a side-effecting step;
- after step outcome is committed;
- when waiting for permission/input;
- after retry scheduling;
- at terminal completion/failure/cancellation;
- periodically for long-running steps if meaningful progress state exists.

### 15.3 Recovery rules

On restore:

1. Load the exact definition revision.
2. Verify definition content hash.
3. Rebuild state from snapshot plus later events.
4. Reconcile any delegated session/run still active.
5. Do not rerun a durably succeeded step.
6. For a step marked `Running` with uncertain side effects, enter a reconciliation state rather than blindly retrying.
7. Re-emit unresolved permission/input requests to subscribers without creating duplicate IDs.

The MVP can initially support recovery only at step boundaries, provided that limitation is explicit and running-step restoration fails safely.

---

## 16. Security and trust boundaries

### 16.1 Trust levels

```text
Highest trust: harness invariants and compiled policies
High trust: published orchestration definition
Medium trust: authenticated user input
Untrusted: model output, tool output, files, web content, upstream step output
```

Being produced by a previous workflow step does not make content trusted.

### 16.2 Required safeguards

- Explicit tool allow lists per step.
- Workspace/path policy enforcement below the model layer.
- Schema validation of all structured boundaries.
- No executable expressions in the initial definition format.
- Redaction policy for persisted events and artifacts.
- Permission correlation by stable IDs.
- Attempt correlation to reject stale completions.
- Definition validation before any side effects.
- Capability validation before model requests.
- Bounded context and output sizes.
- No secret values embedded directly in workflow definitions.

---

## 17. Observability and evaluation

A workflow trace should answer:

- Which definition revision ran?
- Why did each node become ready?
- What exact input bindings were used?
- Which model and parameters were selected?
- Which tools were visible and called?
- Which permissions were requested and resolved?
- What output failed validation and why?
- Which transition was selected?
- Why was a retry admitted or denied?
- What time, tokens, tool calls, and cost did each step consume?
- Which evidence supports completion?

Recommended metrics:

```text
orchestration completion rate
step success rate by node kind
attempts per successful step
schema validation failure rate
verification failure rate
permission denial rate
tool calls per run
model requests per run
latency and cost by step/definition revision
budget exhaustion rate
cancel and pause latency
restoration success rate
```

These metrics allow orchestration revisions to be evaluated independently from tool implementation changes.

---

## 18. Integration boundaries and proposed modules

A clean crate/module split would be:

```text
harness-protocol
  orchestration definitions, commands, events, IDs, results

harness-orchestration-core (new)
  deterministic run state, compiler validation model, reducer,
  transition evaluation, ready-step calculation

harness-runtime
  orchestration runner, step executors, agent adapter,
  policy/tool-scope integration, cancellation, event bridge

harness-session-store or dedicated orchestration store
  durable orchestration events and snapshots

harness-engine
  builder/configuration, default orchestration registration,
  public start/resume/control API
```

An alternative is to initially place deterministic orchestration logic inside `harness-core`. A separate crate is preferable if orchestration types grow substantially, because agent state and workflow state are related but distinct domains.

### 18.1 Suggested runtime API

```rust
#[async_trait]
pub trait OrchestrationService: Send + Sync {
    async fn start(
        &self,
        definition: OrchestrationDefinitionRef,
        input: serde_json::Value,
    ) -> Result<OrchestrationRunId, OrchestrationError>;

    async fn pause(&self, run_id: OrchestrationRunId) -> Result<(), OrchestrationError>;
    async fn resume(&self, run_id: OrchestrationRunId) -> Result<(), OrchestrationError>;
    async fn cancel(&self, run_id: OrchestrationRunId) -> Result<(), OrchestrationError>;
    async fn snapshot(&self, run_id: OrchestrationRunId) -> Result<OrchestrationSnapshot, OrchestrationError>;
}
```

### 18.2 Engine builder compatibility

Orchestration should be optional:

```rust
Harness::builder()
    .backend(backend)
    .tools(registry)
    .workspace(workspace)
    .orchestration(OrchestrationConfig::default())
    .build()
```

Without orchestration configuration, existing direct session behavior remains unchanged.

---

## 19. Future visual designer compatibility

No UI behavior is specified here, but the runtime document must be visually authorable later.

The future designer should edit the same semantic concepts:

- typed nodes;
- typed input/output ports;
- control edges;
- data bindings;
- per-node tool scope;
- model/execution parameters;
- retry and timeout policy;
- output schema selection;
- validation diagnostics;
- definition revision and publication state.

Editor-only state should be separate:

```json
{
  "metadata": {
    "editor": {
      "position": { "x": 420, "y": 180 },
      "collapsed": false,
      "color": "agent"
    }
  }
}
```

The runtime must never interpret position, color, selection, or visual edge routing as execution semantics.

A visual editor is therefore a projection and authoring tool for the orchestration definition—not the orchestration engine itself.

---

## 20. Minimal delivery plan

### Phase 0 — contract spike

- Define IDs, definition types, statuses, commands, effects, events, and results.
- Create JSON fixtures for the default orchestration.
- Validate serialization and versioning.
- Decide JSON Schema library and supported draft.

### Phase 1 — deterministic orchestration core

- Definition validation/compiler.
- Run and step state reducer.
- Ready-step selection.
- Success/failure transitions.
- Bounded retry.
- Final output completion gate.
- Exhaustive transition tests.

No model or tool integration is needed to test this phase.

### Phase 2 — runtime and default workflow

- Async orchestration runner.
- Input, agent, deterministic verify, and output executors.
- Adapter to `SessionRuntime`/child-agent execution.
- Step-scoped tool registry view.
- `ResponseFormat::JsonSchema` integration.
- Host-side schema validation.
- Cancellation and permission-state projection.

### Phase 3 — durability and recovery

- Orchestration event commit sequence.
- snapshots and checkpoints;
- restoration at safe step boundaries;
- agent/session event correlation;
- terminal persistence guarantees.

### Phase 4 — observability and evaluation

- traces and metrics;
- replay tooling;
- workflow fixture suite;
- cost/latency/quality comparisons by definition revision.

### Phase 5 — expanded semantics

Only after the default layer is stable:

- standalone conditions;
- human approval nodes;
- deterministic transforms;
- parallel branches and joins;
- subflows;
- independent verifier agents;
- eventually, a visual authoring interface.

---

## 21. Acceptance criteria for the first orchestration layer

The first implementation is complete when:

1. Existing direct sessions continue working unchanged.
2. A versioned default orchestration definition can be loaded and compiled.
3. Invalid graphs and invalid schemas are rejected before execution.
4. A request can run through `Input → Agent → Verify → Output`.
5. The agent step uses the existing backend, tools, permissions, cancellation, and events.
6. Step-specific tool visibility is enforced at description and execution time.
7. Agent output is validated against a step JSON Schema.
8. Final output is validated against the definition's selected output schema.
9. Invalid structured output produces a bounded retry or deterministic failure.
10. Run and step state can be inspected without parsing chat text.
11. Every agent event used by orchestration is correlated to a node and attempt.
12. Cancellation propagates to active delegated work.
13. Completed steps are not repeated after a safe-boundary restoration.
14. Completion is emitted only after the final result is durably persisted.
15. Unit tests cover every legal state transition and reject illegal transitions.

---

## 22. Open decisions before implementation

These decisions should be made explicitly during review:

1. **Agent isolation:** Should the default use one isolated child agent per attempt, or a shared root session?
   - Recommendation: isolated child per attempt.

2. **Schema draft/library:** Which JSON Schema draft and Rust validator will be supported?
   - Recommendation: choose one draft and reject unsupported keywords rather than partially interpreting them.

3. **Definition storage:** Store definitions with session data, in workspace files, or in a dedicated registry?
   - Recommendation: workspace-visible versioned definitions plus immutable run snapshots/hashes.

4. **Orchestration durability:** Extend the current session store or create a sibling store?
   - Recommendation: share commit/durability infrastructure, but use distinct orchestration event and snapshot types.

5. **Agent result extraction:** What is the authoritative structured result from an agent run?
   - Recommendation: introduce an explicit structured completion payload rather than reconstructing it from streamed text.

6. **Tool scoping:** Does `ToolRegistry` gain a filtered-view API, or does runtime wrap it?
   - Recommendation: a wrapper implementing the existing trait, backed by an immutable allow list.

7. **Backend without structured output:** Reject, or permit host-validated JSON fallback?
   - Recommendation: configurable per node, strict rejection by default for contract-critical outputs.

8. **Running-step restoration:** What guarantees exist for uncertain side effects?
   - Recommendation: restore only at safe step boundaries in MVP and fail closed for indeterminate active attempts.

9. **Verification contract:** Which deterministic checks are built in initially?
   - Recommendation: schema conformance, artifact existence, required status, and budget compliance.

10. **Definition expressions:** Which binding syntax is supported?
    - Recommendation: JSON Pointer first; no arbitrary expression evaluator in MVP.

---

## 23. Recommended decisions

To keep the first orchestration layer genuinely simple:

- Use a declarative, versioned DAG definition.
- Implement only `Input`, `Agent`, deterministic `Verify`, and `Output` node kinds.
- Execute one node at a time.
- Use isolated child agents for agent steps.
- Use explicit tool allow lists.
- Use JSON Pointer for data bindings.
- Support success/failure edges only.
- Model retry as bounded step policy, not a graph cycle.
- Use provider structured output when available and always validate host-side.
- Require a final output schema.
- Persist immutable attempts and structured events.
- Restore only at safe step boundaries initially.
- Keep orchestration optional so current session behavior remains stable.

This gives Rusty a real orchestration layer without prematurely committing to a complex visual language or workflow engine.

---

## 24. Final architecture statement

The orchestration layer should not replace the current harness. It should organize it.

```text
Orchestration definition
  defines steps, contracts, policies, and legal transitions

Orchestration core
  deterministically decides what may happen next

Orchestration runtime
  schedules steps, enforces budgets/policies, and records outcomes

SessionRuntime + AgentRunner
  perform model/tool/permission/child-agent execution

Backends and tools
  perform external work
```

The model remains the semantic decision-maker inside explicitly bounded agent steps. The harness becomes the operational authority over the full workflow.

That boundary is what makes future visual orchestration possible: the canvas will eventually edit a stable executable document whose behavior is already defined, validated, observable, and durable.
---

## 25. Implementation status

The first orchestration layer (§21) is implemented. Decisions from §22 were taken as recommended unless noted.

### 25.1 Where it lives

| Concern | Location |
|---|---|
| Definition types, `ToolScope`, `DefinitionStatus`, policies | `harness-core/src/orchestration/definition.rs` |
| Compiler/validator (all issues in one report, content hash, retry spans) | `harness-core/src/orchestration/compiler.rs` |
| Definition registry (immutable published revisions, drafts gated) | `harness-core/src/orchestration/registry.rs` |
| Pure run-state reducer, events, results | `harness-core/src/orchestration/state.rs` |
| Built-in `rusty.default` definition | `harness-core/src/orchestration/default.rs` |
| Async runner, `OrchestrationHandle`, commit-before-publish | `harness-runtime/src/orchestration/runner.rs` |
| Input / Verify / Output executors, agent-step contract, artifact resolvers | `harness-runtime/src/orchestration/steps.rs` |
| Host-side JSON Schema validation | `harness-runtime/src/orchestration/schema.rs` |
| Durable events + snapshots (in-memory, file) | `harness-runtime/src/orchestration/store.rs` |
| Isolated, tool-scoped agent attempts | `harness-runtime/src/session_agent_executor.rs`, `scoped_tools.rs` |
| Public API (`HarnessBuilder::orchestration`, `SessionHandle::{start,run,resume}_orchestration`) | `harness-engine/src/orchestration.rs` |

Orchestration stayed inside `harness-core` (the §18 alternative) rather than a new crate; it is a self-contained module and can be split out later without API changes.

### 25.2 Decisions taken

- **Isolation:** one fresh session per agent attempt (`IsolatedChild`). `SharedSession` is accepted by the schema but rejected at run time.
- **Schema draft:** a fixed keyword subset (`SUPPORTED_SCHEMA_KEYWORDS`); any other keyword is rejected at compile time, never partially interpreted. `$ref`, `pattern`, `oneOf`/`anyOf` and array-valued `type` are not supported.
- **Bindings:** JSON Pointer only. The compiler rejects a binding to a node that is not guaranteed to have run.
- **Tool scoping:** a wrapper over `ToolRegistry` plus a backend view that filters descriptors. `Inherit` resolves at run start to the parent session's enabled, registered tools and is recorded on each attempt. Allow-listed tools that do not exist fail the run before any side effect. Each tool keeps the parent's permission policy; a tool the parent does not list defaults to `Ask`.
- **Structured output:** `Require` (default) rejects a backend without `structured_output` before any request; `HostValidatedFallback` asks for JSON text. Host validation always runs.
- **Result extraction:** the last completed assistant message of the root agent, not the concatenation of every delta.
- **Verify → Execute:** `VerifyNodeConfig.retry_target`, bounded by the target's own `retry` policy (which must list `verification_failed`). Validation/verification errors are passed to the next attempt as data (`StepRun.feedback`).
- **Retry classification:** only `BACKEND_ERROR` rate-limit/timeout failures, step timeouts, invalid structured output and verification failures are retryable.
- **Durability:** every transition is committed (events appended, snapshot replaced) before its events are published or its effects dispatched. A persistence failure halts the run. `RunCompleted` is therefore never observed before it is durable.
- **Restoration:** at step boundaries. An interrupted agent attempt fails closed with `INDETERMINATE_ATTEMPT`; interrupted deterministic steps (input/verify/output) are replayed. A changed definition (content hash) is refused.
- **Budgets:** attempts, model requests, tool calls, tokens, cost (checked at admission, unknown values tracked as unknown rather than zero), and elapsed time (enforced by the runner).

### 25.3 Integration notes

- **Permissions:** a delegated agent's permission requests surface as `PermissionRequested` orchestration events with the run in `WaitingForPermission`. Resolve them with `OrchestrationHandle::resolve_permission`, not the parent `SessionHandle`. As in direct sessions, only the step's root agent's requests are resolvable.
- **Observability:** `OrchestrationHandle::subscribe` yields committed orchestration events and live agent events tagged with `StepCorrelation` (run, definition, revision, node, attempt, delegated session/agent/run). `snapshot()`/`watch()` expose run and step state directly.

### 25.4 Still deferred (per §8 / §20 phase 5)

`Approval`, `Transform`, `Condition`, `Parallel`/`Join`, `Subflow` and explicit `Fail` nodes; `WaitingForInput`; restricted expression edges; concurrency above one; backoff delays between retries; independent verifier agents; metrics export and replay tooling (§17, phase 4).

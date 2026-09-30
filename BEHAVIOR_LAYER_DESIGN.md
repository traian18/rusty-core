# Agent Behavior Layer — Profiles, Loop Rules, and Completion Gates

**Status:** Design accepted (§14). **Phases 1–5 implemented** (§15)
**Builds on:** `ORCHESTRATION_LAYER_DESIGN.md` (workflow level), `AGENT_BEHAVIOR_COMPARISON.md` (research)
**Scope:** Behaviour *inside* one agent loop. Harness/runtime only; the canvas is a future editor of the same JSON.

---

## 1. Summary

The orchestration layer decides **which step runs**. Inside a step, the model decides everything: which tool to call, when to stop, what to say. Claude Code, Codex, and OpenCode show that the model's behaviour cannot be controlled directly, but it can be shaped reliably by three things:

1. **What it sees**: instructions, per-turn reminders, and context attached to tool results.
2. **What it may do**: which tools are visible and callable, which calls are denied (with a reason the model reads), and which need approval.
3. **When it may stop**: a completion gate that refuses "done", sends feedback back, and keeps the loop going.

All three change in response to **events in the loop**.

This document adds that as a declarative, versioned JSON document: a **behavior profile**. A profile is:

- a bundle of instructions + tools + execution params + limits;
- plus **rules** of the form *on event, when condition, do action*;
- plus an optional **completion gate**.

Profiles attach to a session or to an orchestration `agent` node, and can switch mid-run (plan → build).

```text
Workflow (orchestration layer)           which step runs
  └── Agent node ── profile ─────────────  how the agent inside that step behaves
         ├── instructions / tools / params / limits
         ├── rules: on PreToolUse | PostToolUse | BeforeModelRequest | ...
         └── completion gate: declarative checks + evaluators
```

The event vocabulary deliberately matches Claude Code and Codex hooks (`PreToolUse`, `PostToolUse`, `Stop`, …; research §6.1), so the concepts transfer and existing hook configs could later be imported.

---

## 2. Goals and non-goals

### Goals

1. Named, versioned **profiles**: instructions, tool scope, execution params, turn/tool limits.
2. **Declarative rules** on loop events that can:
   - inject context;
   - allow, deny, or require approval for tool calls;
   - enforce ordering ("fetch before edit");
   - detect loops;
   - switch profile.
3. A **completion gate** that can reject completion with feedback, up to a bounded number of continuations.
4. **Evaluators** for gate checks that need I/O: run a tool, ask a model, or delegate to a verifier agent.
5. **Enforced where possible, visible where not.** Every rule action is labelled as either:
   - **enforced**: the harness guarantees it;
   - **guidance**: the model is told, and may not comply.
6. **Deterministic core.** Rule evaluation is pure and lives in `harness-core`, next to `Agent::apply`. Async evaluators go through the effect/command round-trip.
7. **Durable.** Profile identity and rule counters survive checkpoint/restore.
8. **Every behaviour decision is observable** as an event: rule fired, context injected, call denied, gate verdict.
9. **Backward compatible.** Every agent runs under a profile. When none is chosen it is the built-in `rusty.default` (§5.3), which reproduces today's behaviour exactly.
10. **JSON is the only authoring format**, so the UI can edit every field. A published JSON Schema describes it (§5.4).

### Non-goals (this version)

- A graph of individual model calls. The research shows none of the three tools does this; the workflow graph already covers multi-step flows.
- Arbitrary scripting in rules. Conditions are a small declarative language. Shell `command` evaluators come in the last phase, behind trust.
- Rewriting provider wire formats, or exposing private reasoning.
- A canvas UI.

---

## 3. Where behaviour is decided today (code facts)

| Concern | Current code | Consequence for this design |
|---|---|---|
| Request assembly | `Agent::execution_request` (`harness-core/src/transitions.rs`): system prompt, full transcript, **all enabled tools**, `execution_params` | Profile tool scope, per-turn reminders, and params are applied here. It is pure and already has the agent state. |
| Tool permission | `Agent::tool_requested`: looks up `ToolCapability.policy.permission`, then `ExecuteTool`, `RequestPermission`, or `tool_failed(PermissionDenied)` | PreToolUse rules run here, before the static policy. |
| Deny feedback | `ToolError` is `ExecutionFailed \| PermissionDenied \| Timeout \| Internal`, with no message; the model sees `"PermissionDenied"` | Needs a variant carrying a reason, so a deny can teach the model. |
| Tool result in transcript | `record_tool_result` stores `ToolResultSummary { has_error, output_preview: String }` | PostToolUse context is attached to the result text. This is where Claude Code puts `additionalContext`, and it keeps the transcript valid. |
| Completion | `backend_event(ExecutionEvent::Completed)` with `finish_reason != "tool_use"` goes **straight to `Idle` + `FinishRun`** | The completion gate must live in the core state machine; the runner alone cannot intercept it. |
| Async I/O before a request | `ContextProvider::assemble(request, workspace)` via `ContextAssemblingBackend`, which wraps the **session** backend (all agents) | Keep it for workspace-aware context (skills, compaction). Behaviour rules do not go here: the request carries no agent identity or behaviour state. |
| Root system prompt | Root agent is constructed with `system_prompt: String::new()`; instructions arrive via `ContextProvider` | Profile instructions become the agent's `system_prompt`. |
| Persistence | `StoredAgentState` (`harness-session-store`) + `stored_agent_state` projection | Needs an additive `behavior` field. |
| Workflow step | `AgentNodeConfig` (orchestration) + `IsolatedSessionAgentExecutor` | Gains `profile: Option<ProfileRef>`. |

---

## 4. Concepts

```text
BehaviorProfile      immutable, versioned: instructions, tools, params, limits, rules, gate
ProfileRegistry      id+revision → compiled profile (same model as DefinitionRegistry)
CompiledProfile      validated profile + pre-resolved matchers + content hash
BehaviorState        per-agent mutable: active profile ref, counters, fired-rule bookkeeping,
                     pending ephemeral injections, gate continuations
Rule                 on <event> [when <condition>] do <action>
Evaluator            async check used by the gate: tool | model | agent | (later) command
ChildBehaviorPolicy  how a spawned child agent gets its behaviour, defined separately from
                     the profile and resolved by a pluggable resolver (§8a)
```

The same split as orchestration: **definitions are immutable, state is mutable**, and editing a profile creates a new revision.

---

## 5. The profile document

### 5.1 Shape

```json
{
  "schema_version": 1,
  "id": "careful-coder",
  "revision": 3,
  "name": "Careful coder",
  "status": "published",

  "instructions": {
    "mode": "append",
    "text": "You fix bugs with the smallest correct change. Always read before editing."
  },

  "tools": { "type": "allow_list", "tools": ["fs.read", "workspace.search", "fs.edit", "smart_fetch", "run_tests"] },
  "tool_overrides": {
    "fs.edit":   { "permission": "ask" },
    "smart_fetch": { "description_append": "Prefer this over reading many files one by one." }
  },

  "execution": { "model": null, "temperature": 0.2, "reasoning_effort": "medium" },

  "limits": {
    "max_turns": 40,
    "max_tool_calls": 120,
    "final_turn_prompt": "This is your last turn. Summarize what is done and what remains."
  },

  "rules": [ /* §6 */ ],
  "completion_gate": { /* §7 */ },
  "children": { "type": "inherit" },

  "metadata": { "editor": { "position": { "x": 0, "y": 0 } } }
}
```

### 5.2 Field semantics

| Field | Enforced / guidance | Semantics |
|---|---|---|
| `instructions.mode` | guidance | `append` adds to the session's system prompt; `replace` replaces it (OpenCode per-agent prompt; Claude Code output style without `keep-coding-instructions`) |
| `instructions.variants` *(optional)* | guidance | Per-model-family text, e.g. `{"claude": "...", "gpt": "..."}`. OpenCode's lesson: models need different wording |
| `tools` | **enforced** | Reuses orchestration `ToolScope` (`none` / `allow_list` / `inherit`). Filtered in `execution_request` (description) **and** in `tool_requested` (execution). A hidden tool is never executable by name |
| `tool_overrides.permission` | **enforced** | Tightens only: a profile may move `allow → ask → deny`, never loosen the session policy |
| `tool_overrides.description_append` | guidance | Appended to the tool description sent to the model (OpenCode `tool.definition`) |
| `execution` | **enforced** | Partial `ExecutionParams` overlay while the profile is active |
| `limits.max_turns` / `max_tool_calls` | **enforced** | Counted in `BehaviorState`. At the limit: the final-turn prompt is sent (OpenCode `MAX_STEPS_PROMPT`) with **no tools offered**, so the model must answer in text. After that turn the run ends normally |
| `rules` | per action | §6 |
| `completion_gate` | **enforced** (the model cannot finish until it passes or the gate gives up) | §7 |
| `children` | **enforced** | How spawned children get their behaviour. A reference to a separate child policy; only `inherit` is implemented (§8a) |
| `metadata` | — | Editor data (layout, colours, notes). Preserved, never interpreted |

### 5.3 Built-in default profile

`rusty.default` is compiled into `harness-core`, the same way `default_orchestration_definition()` is. It is always registered and cannot be overwritten, because published revisions are immutable.

```json
{
  "schema_version": 1,
  "id": "rusty.default",
  "revision": 1,
  "name": "Default",
  "status": "published",
  "instructions": { "mode": "append", "text": "" },
  "tools": { "type": "inherit" },
  "tool_overrides": {},
  "execution": {},
  "limits": {},
  "rules": [],
  "completion_gate": null,
  "children": { "type": "inherit" }
}
```

It is **behaviour-neutral on purpose**: no rules, no gate, no limits, inherited tools and params. Running every agent under it therefore changes nothing observable, which keeps the existing test suite as the compatibility proof. Opinionated built-ins (e.g. `rusty.plan` / `rusty.build`) can ship later as *additional* profiles without touching the default.

**Resolution order** for the profile an agent starts with:

1. explicit reference: orchestration node `profile`, `SessionBuilder::profile`, or spawn policy (§8a)
2. the workspace default: `.rusty/profiles/config.json` → `{ "default": { "id": "...", "revision": n } }`
3. the built-in `rusty.default`

A reference that doesn't resolve is an error, never a silent fallback to the default.

### 5.4 Storage and the JSON contract

JSON is the canonical and only authoring format, so everything is editable in the UI.

- **Workspace profiles** live in `.rusty/profiles/<id>.json`, one document per revision (or a `revisions` array), versioned with the repo. They load behind the same workspace trust as skills.
- **The host registry** (`ProfileRegistry`) holds:
  - the built-ins;
  - workspace profiles, loaded at session start;
  - profiles the host registers programmatically, e.g. from the UI.
- **Precedence** for the same id+revision: host > workspace > built-in. The built-in `rusty.default@1` is reserved and cannot be shadowed.
- **A JSON Schema for the profile format is generated from the Rust types** and published at `schema/behavior-profile-v1.schema.json`, next to the existing `schema/protocol-v2.schema.json`. The UI validates against it while editing. The compiler (§9.1) remains the authority on meaning; the schema only checks shape.
- **Unknown fields are rejected, except under `metadata`.** Editor layout, colours and notes go in `metadata` (on the profile, on each rule, and on each gate check). The runtime preserves `metadata` and ignores it.
- **Every rule and gate check has a stable `id`.** The UI keys edits on it, and events report it.

---

## 6. Rules

### 6.1 Events

Names follow Claude Code/Codex where the concept matches.

| Event | Fires | Evaluated in | Available data |
|---|---|---|---|
| `RunStart` | a run starts (≈ `SessionStart` / `UserPromptSubmit`) | `start_run` | user input text |
| `BeforeModelRequest` | before **every** backend request | `execution_request` | turn number, counters, last tool results |
| `PreToolUse` | model requested a tool, before permission and execution | `tool_requested` | tool name, args |
| `PostToolUse` | tool succeeded | `record_tool_result` | tool name, args, output preview |
| `PostToolUseFailure` | tool failed or was denied | `record_tool_result` | tool name, args, error |
| `Stop` | model produced a final answer (no tool calls) | `backend_event(Completed)` | final assistant text |
| `ProfileEntered` | this profile became active (start or switch) | switch handler | previous profile id |

### 6.2 Conditions (`when`)

A small, pure, typed language. There are **no expressions and no regex engine in core**. Globs support three wildcards:
- `*` matches within a path segment;
- `**` matches across segments;
- `?` matches one character.

Each condition is an object with exactly one key. Comparisons take any of `eq`, `gte`, `lte`, and every bound given must hold.

```json
{ "tool": "fs.edit" }                                              // glob; or a list: ["fs.*", "workspace.*"]
{ "arg": { "pointer": "/path", "glob": "src/billing/**" } }        // JSON Pointer into tool args
{ "arg": { "pointer": "/command", "contains": "rm -rf" } }         // also "equals": <json>
{ "result_contains": "error" }                                     // post-tool events only
{ "turn": { "gte": 3 } }
{ "calls": { "tool": "smart_fetch", "eq": 0 } }                    // executed calls this run
{ "turns_since_call": { "tool": "infer_decision", "gte": 4 } }
{ "since_last_call": { "of": "fs.edit", "called": "run_tests", "eq": 0 } }   // ordering; false if `of` never ran
{ "repeated_call": { "gte": 3 } }                                  // same tool + args in a row (PreToolUse)
{ "all": [ ... ] }  { "any": [ ... ] }  { "not": { ... } }
```

"Executed" means the tool actually ran. Calls refused by the harness or denied by the user don't count. `profile_entered_from` arrives with profile switching (phase 4).

Every condition is a function of `BehaviorState` + the event payload, so it can be tested exhaustively.

### 6.3 Actions (`do`)

| Action | Allowed on | Kind | Effect |
|---|---|---|---|
| `inject` `{text, placement}` | all | guidance | Adds context (placement below) |
| `deny` `{reason}` | `PreToolUse` | **enforced** | The call is not executed. The tool result is an error whose text **is the reason**, so the model reads why and can adapt |
| `ask` `{reason?}` | `PreToolUse` | **enforced** | Forces the permission flow for this call, even if the policy is `allow` |
| `allow` `{}` | `PreToolUse` | **enforced** | Skips the approval prompt for this call. Can only apply where the session policy is `ask`; can never override `deny` |
| `stop_run` `{reason}` | all | **enforced** | Fails the run with `code: "BEHAVIOR_STOP"` |
| `switch_profile` `{profile}` | all except `Stop` | **enforced** (tools/params) + guidance (announcement) | Activates another profile from the next request, with an automatic announcement (§8) |
| `rewrite_args` `{merge}` | `PreToolUse` | **enforced** | JSON merge-patch into the args. *Phase 5* |
| `rewrite_result` `{append \| replace}` | `PostToolUse` | **enforced** | Changes what the model sees as the result. *Phase 5* |

**First match wins for `PreToolUse` decisions**: rules are evaluated in order, and the first `deny` / `ask` / `allow` decides. `inject` actions accumulate. Every rule can carry `"max_fires": n` and `"id"`, so reminders don't repeat forever.

### 6.4 Placement of injected context

Transcript validity (tool result must follow tool use, roles must alternate) decides where text can go. These placements mirror what the researched tools do.

| Placement | Stored in transcript? | Where | Used for |
|---|---|---|---|
| `with_result` (default on tool events) | yes | Appended to that tool result's `output_preview`, wrapped as `<system-reminder>` | "You edited code; run the tests." (Claude Code `additionalContext`) |
| `next_request` (default on `BeforeModelRequest`) | **no** | Appended to the last message **of the outgoing request only**, not the canonical transcript | Mode reminders re-sent each turn while a condition holds (OpenCode synthetic parts) |
| `persistent` | yes | A new user message; only legal at a run boundary (`RunStart`, gate continuation) | Durable instructions |

Injected text is always wrapped:

```text
<system-reminder source="profile:careful-coder@3 rule:test-after-edit">...</system-reminder>
```

The wrapper lets the model, a UI, and replay tell harness text apart from user text. **Interpolation is limited to trusted values**: `{turn}`, `{tool}`, `{profile}`. Model-produced args are never interpolated into instructions.

### 6.5 Example: "each loop makes specific steps"

This covers your earlier case: always decide with `infer_decision`, always fetch with `smart_fetch` before acting, and evaluate at the end.

```json
"rules": [
  { "id": "decide-first", "on": "BeforeModelRequest",
    "when": { "calls": { "tool": "infer_decision", "eq": 0 } },
    "do": { "inject": { "text": "Start by calling infer_decision to choose an approach." } } },

  { "id": "fetch-before-edit", "on": "PreToolUse",
    "when": { "all": [ { "tool": "fs.edit" }, { "calls": { "tool": "smart_fetch", "eq": 0 } } ] },
    "do": { "deny": { "reason": "Gather the data with smart_fetch before editing." } } },

  { "id": "redecide", "on": "BeforeModelRequest",
    "when": { "turns_since_call": { "tool": "infer_decision", "gte": 6 } },
    "max_fires": 2,
    "do": { "inject": { "text": "Re-evaluate your approach with infer_decision before continuing." } } },

  { "id": "test-after-edit", "on": "PostToolUse", "when": { "tool": "fs.edit" },
    "do": { "inject": { "text": "Run run_tests before claiming completion." } } },

  { "id": "no-loops", "on": "PreToolUse", "when": { "repeated_call": { "gte": 3 } },
    "do": { "deny": { "reason": "You have made this exact call 3 times. Change approach." } } }
]
```

Here `fetch-before-edit` and `no-loops` are **guaranteed**. `decide-first` and `redecide` are **strong guidance**, and the gate (§7) can make them guaranteed at the end.

---

## 7. Completion gate

### 7.1 Semantics

```json
"completion_gate": {
  "checks": [
    { "id": "tested", "require": { "not": { "since_last_call": { "of": "fs.edit", "called": "run_tests", "eq": 0 } } },
      "feedback": "You changed code after the last test run. Run run_tests." },
    { "id": "tests-green", "evaluator": { "type": "tool", "tool": "run_tests", "args": {} },
      "feedback_from": "evaluator" },
    { "id": "judge", "evaluator": { "type": "model", "instructions": "Did the answer fully address the request? ...",
                                      "model": "claude-haiku-4-5-20251001" } }
  ],
  "max_continuations": 3,
  "on_exhausted": "fail"
}
```

- **Declarative checks** (`require`) are evaluated synchronously in core against `BehaviorState`.
- **Evaluator checks** (`evaluator`) need I/O. The core emits an effect, the runner evaluates, and the verdict comes back as a command.
- **Any failed check** → the feedback of every failed check becomes one `persistent` user message:

  ```text
  <system-reminder source="gate">Completion rejected: ...</system-reminder>
  ```

  and the model runs again. This is Claude Code's `Stop` → `decision: block` + `reason`.
- `max_continuations` bounds this. When exhausted, `on_exhausted` is either:
  - `fail`: the run fails with `GATE_EXHAUSTED`;
  - `accept`: the run completes, flagged `gate_passed: false` in the result.

### 7.2 State machine change

In `backend_event(Completed)`, the non-tool branch becomes:

```text
no profile / no gate          → Idle + FinishRun               (unchanged)
declarative checks fail       → append feedback message → ExecuteBackend        (no I/O)
evaluators configured         → status Verifying, emit AgentEffect::EvaluateCompletion
AgentCommand::CompletionEvaluated { run_id, attempt, verdicts }
   all pass                   → Idle + FinishRun
   any fail, budget left      → append feedback → WaitingForBackend + ExecuteBackend
   any fail, exhausted        → fail(GATE_EXHAUSTED) or FinishRun{gate_passed:false}
Cancel while Verifying        → CancelEvaluation effect, normal cancel path
```

New protocol items (additive):

- `AgentStatus::Verifying`
- `AgentEffect::EvaluateCompletion`, `AgentEffect::CancelEvaluation`
- `AgentCommand::CompletionEvaluated`

Stale verdicts (wrong `run_id` or evaluation attempt) are ignored, the same guard the orchestration reducer uses.

### 7.3 Evaluators (runtime)

```rust
#[async_trait]
pub trait CompletionEvaluator: Send + Sync {
    async fn evaluate(&self, input: EvaluationInput, cancel: CancellationToken) -> Verdict;
}
pub enum Verdict { Pass, Fail { feedback: String }, Error { message: String } }
```

| Type | Runs | Notes |
|---|---|---|
| `tool` | a registered tool, directly, through the **same permission policy** as the agent | Pass when the tool succeeds (`is_error == false`); the output becomes the feedback |
| `model` | one backend request with a JSON Schema response `{passed: bool, feedback: string}` | Gets the final answer plus a bounded tail of the transcript. The Claude Code `prompt` hook equivalent. Host-validated like orchestration outputs |
| `agent` | an isolated verifier session (reuses `IsolatedSessionAgentExecutor`) | For expensive checks. The Claude Code `agent` hook equivalent |
| `command` | shell command, Claude Code/Codex hook I/O contract | **Phase 5 only**, requires explicit trust, never from a model-authored profile |

`Verdict::Error` counts as a failure with a generic message by default. `"error_policy": "pass"` can override it per check.

---

## 8. Profile switching (modes)

`switch_profile` is how plan → build works. The OpenCode source shows the switch must **announce itself**; otherwise the model keeps acting in the old mode.

1. The action records `pending_switch = Some(target)` in `BehaviorState`.
2. At the next `execution_request`:
   - activate the target: instructions, tool scope, params, reset per-profile counters;
   - fire `ProfileEntered` rules;
   - send a one-shot `next_request` reminder: *"Mode changed from plan to build. Available tools: …"*.
3. Emit a `ProfileChanged` event.

Hosts can also switch explicitly with `AgentCommand::SetProfile { profile }`, the equivalent of Tab in OpenCode or Shift+Tab in Claude Code. The same `ProfileRegistry` validates the target. An unknown profile is rejected; it is never silently ignored.

---

## 8a. Child agents: a separate, pluggable behaviour policy

How a child agent gets its behaviour is **its own concept, defined separately from the profile**. Today the answer is simply "inherit". Later it should cover inheriting only specific behaviour and defining child *lifecycles* (when a child may start, what it reports, when it is stopped). Keeping it separate means that growth doesn't reshape the profile format.

### 8a.1 In JSON

A profile only *references* a child policy:

```json
"children": { "type": "inherit" }
```

The `type` is an open, tagged set. Only `inherit` is implemented now. These shapes are reserved so the UI and format don't break when they arrive:

| `type` | Meaning | Status |
|---|---|---|
| `inherit` | The child runs the parent's **currently active** profile (after any switch), with its own fresh `BehaviorState`: counters, fired rules, gate continuations | **implemented** |
| `named` | `{ "profile": {id, revision} }`: the child runs a specific profile | reserved |
| `derived` | inherit, plus selected overrides (e.g. drop the gate, narrow tools) | reserved |
| `policy` | `{ "ref": {id, revision} }`: a separately versioned `ChildBehaviorPolicy` document for inheritance and lifecycle rules | reserved |

The compiler **rejects reserved types** with `unsupported_child_policy`, the same fail-closed treatment the orchestration compiler gives unimplemented node kinds.

### 8a.2 In code

Resolution goes through a trait, so hosts can plug in their own behaviour before the JSON grows:

```rust
/// Decides the behaviour a spawned child starts with.
pub trait ChildBehaviorResolver: Send + Sync {
    fn resolve(
        &self,
        parent: &BehaviorState,          // includes the parent's active profile
        spec: &SpawnAgentSpec,           // what the parent asked for
        registry: &ProfileRegistry,
    ) -> Result<ChildBehavior, ChildPolicyError>;
}

pub struct ChildBehavior {
    pub profile: Arc<CompiledProfile>,
    // Reserved for lifecycle hooks; empty for `inherit`.
    pub lifecycle: ChildLifecycle,
}
```

- **The default resolver is `InheritParentProfile`**, which implements the `inherit` type.
- The runtime calls the resolver at the single place children are created (`AgentRunner::spawn_agent`), and the child's `BehaviorState` is built from the result.
- The resolver is chosen per session via `SessionBuilder::child_behavior_resolver(..)`, defaulting to one that dispatches on the profile's `children.type`.
- It is stored on the `SessionRuntime`, next to the `AgentSupervisor` that already mediates child creation.

**Implication.** With `inherit`, a child also inherits the parent's completion gate and limits. It enforces them independently, against its own counters. That's usually right, e.g. a child editing code must also run tests. When it isn't, that's the use case for `derived`.

---

## 9. Architecture and code changes

### 9.1 Placement

```text
harness-protocol   ToolError::Denied{reason}; AgentStatus::Verifying; new commands, effects, events
harness-core       behavior/ (new module, pure)
                     definition.rs   profile JSON types (serde), ToolScope reuse
                     compiler.rs     validation → CompiledProfile (all issues in one report, content hash)
                     registry.rs     ProfileRegistry (published immutable, drafts gated)
                     state.rs        BehaviorState (counters, fired rules, pending switch, continuations)
                     rules.rs        pure evaluation: fn evaluate(event, &state, &profile) -> Decisions
                     default.rs      built-in rusty.default profile
                     children.rs     ChildPolicy (JSON), ChildBehaviorResolver trait, InheritParentProfile
                   agent.rs / agent_state.rs    Agent gets behavior: Option<BehaviorState>
                   transitions.rs  calls into behavior at the 6 event points
harness-runtime    behavior/evaluators.rs  CompletionEvaluator impls (tool, model, agent)
                   agent_runner.rs         handle EvaluateCompletion/CancelEvaluation effects
                   session_runtime/projection.rs + restoration.rs   persist/restore behavior state
harness-session-store  StoredAgentState.behavior: Option<Value>  (#[serde(default)])
harness-engine     SessionBuilder::profile(ProfileRef), ::child_behavior_resolver(..);
                   HarnessBuilder profile registry; workspace loader for .rusty/profiles/;
                   SessionHandle::set_profile(..)
schema/            behavior-profile-v1.schema.json (generated from the Rust types)
harness-core::orchestration   AgentNodeConfig.profile: Option<ProfileRef>
```

The core stays synchronous and deterministic. The profile is carried as `Arc<CompiledProfile>` inside `BehaviorState`. Only `(id, revision, content_hash)` + counters are persisted; restore re-resolves through the registry and **fails closed** on a hash mismatch or a missing profile, the same rule as orchestration restore.

### 9.2 Insertion points in `transitions.rs`

| Function | Change |
|---|---|
| `start_run` | fire `RunStart`; apply `persistent` injections; reset per-run counters |
| `execution_request` | apply pending switch; filter `tools` by profile scope; apply `description_append`; overlay `execution`; evaluate `BeforeModelRequest` rules and append `next_request` injections to the **request copy only**; enforce `max_turns` (final-turn prompt, tools emptied); increment turn counter |
| `tool_requested` | before the policy lookup: out-of-scope tool → `deny`; evaluate `PreToolUse` rules → first decision wins (`deny` → `tool_failed(Denied{reason})`, `ask` → the existing permission branch, `allow` → the execute branch when policy is `ask`); update call counters / repeat detector |
| `record_tool_result` | evaluate `PostToolUse` / `PostToolUseFailure`; append `with_result` injections to `output_preview`; update `since_last_call` bookkeeping |
| `backend_event(Completed)` non-tool branch | completion gate (§7.2) |
| new `completion_evaluated`, `set_profile` | new commands |

Each of these is a small call into `behavior::rules`. The transition functions stay readable, and the rule engine is tested on its own.

### 9.3 Events (observability)

New `AgentEvent` variants, all additive:

- `BehaviorRuleFired { rule_id, event, action }`
- `ContextInjected { source, placement, chars }`
- `ToolCallDenied { call_id, rule_id, reason }`
- `ProfileChanged { from, to }`
- `CompletionGateEvaluated { attempt, verdicts, continuing }`

These are what a canvas would light up during replay. They also answer "why did the agent do that?" without parsing chat text.

Protocol surface (additive):

- `schema/protocol-v2.schema.json`
- TypeScript SDK types
- `ProtocolCapabilities::behavior_profiles: bool`

---

## 10. Relationship to the orchestration layer

| | Orchestration (built) | Behavior (this doc) |
|---|---|---|
| Unit | workflow run | one agent loop |
| Decides | which step, retries, final contract | what the model sees, may call, and when it may stop |
| Verification | `verify` node between steps; retry = new attempt, fresh session | completion gate inside the loop; continue in the same context |
| Definition | `OrchestrationDefinition` | `BehaviorProfile` |
| Canvas level | graph of steps | per-agent-node profile editor (rules list, gate, tools) |

An orchestration `agent` node references a profile:

```json
{ "id": "execute", "type": "agent",
  "config": { "instructions": "…", "tools": { "type": "inherit" },
              "profile": { "id": "careful-coder", "revision": 3 } } }
```

The node's tool scope and the profile's tool scope **intersect**, and the stricter one wins. The node's output schema still governs the step result; the gate governs when the agent may produce it.

---

## 11. Security and trust

- **Profiles are high-trust configuration**, like orchestration definitions: authored by the user, or loaded from the workspace behind trust.
- Model output, tool output, and upstream step output are **data**. They can be *matched* by conditions but never become instructions: no interpolation, and evaluator feedback from `model` / `agent` evaluators is wrapped as data in the reminder.
- Profiles can only **tighten** the session's tool policy (`allow → ask → deny`), never loosen it.
- A `model` evaluator is a billed request. It counts toward orchestration budgets when running inside a workflow step.
- `command` evaluators (Phase 5) require explicit per-profile trust and are never enabled by a profile the model can edit.

---

## 12. Delivery plan

Each phase is shippable and testable alone. The phases are ordered by value per effort.

| Phase | Scope | Main tests |
|---|---|---|
| **1. Profiles** | Definition/compiler/registry; **built-in `rusty.default`** + resolution order; **workspace loading** from `.rusty/profiles/`; **generated JSON Schema**; instructions (append/replace/variants); tool scope enforced at description + execution; `tool_overrides.permission`; execution overlay; `max_turns` / `max_tool_calls` + final-turn prompt; **`children: inherit` + `ChildBehaviorResolver`**; `SessionBuilder::profile`; orchestration `AgentNodeConfig.profile`; persistence | Full existing suite passes under `rusty.default`; hidden tool is not executable by name; params overlay reaches `ExecutionRequest`; final turn offers no tools; spawned child runs the parent's active profile with fresh counters; reserved child types rejected; schema validates the default profile and every fixture; restore with changed profile hash fails closed |
| **2. Rules** | Events `RunStart`, `BeforeModelRequest`, `PreToolUse`, `PostToolUse(Failure)`; conditions §6.2; actions `inject`, `deny` (with `ToolError::Denied{reason}`), `ask`, `allow`, `stop_run`; placements; `max_fires`; events §9.3 | Exhaustive `rules.rs` unit tests; deny reason visible in the next request's transcript; `next_request` injection not in the canonical transcript; loop detector |
| **3. Completion gate** | Declarative checks; `Verifying` status; effect/command round-trip; `tool` and `model` evaluators; `max_continuations` / `on_exhausted`; cancellation during verification | Gate rejects → feedback message → another backend call; stale verdict ignored; exhausted → `GATE_EXHAUSTED`; cancel while verifying |
| **4. Profile switching** | `switch_profile` action; `SetProfile` command + `SessionHandle::set_profile`; announcement reminder; `ProfileEntered` | Plan→build: tools and params change on the next request, and the announcement is sent exactly once |
| **5. Advanced** | `rewrite_args` / `rewrite_result`; `agent` evaluator; trusted `command` evaluator with Claude Code/Codex hook I/O; import of `hooks.json`; JSON Schema export of the profile format for the canvas | Contract tests against the Claude Code/Codex hook JSON shapes |

---

## 13. Acceptance criteria (phases 1–4)

1. Every agent runs under a profile; with none chosen it runs the built-in `rusty.default`, and the existing test suite passes unchanged.
2. A published profile can be loaded, compiled, and referenced by id+revision; invalid profiles are rejected with all issues reported.
3. Tools outside a profile's scope are neither described to the model nor executable.
4. A `deny` rule prevents execution, and the model receives the rule's reason as the tool result.
5. Ordering constraints (`since_last_call`, `calls`) can be enforced via `deny`.
6. `BeforeModelRequest` injections reach the request but not the stored transcript; `with_result` injections are stored with the tool result.
7. The completion gate can reject completion; rejected feedback is delivered as a user message and the loop continues, bounded by `max_continuations`.
8. `tool` and `model` evaluators work, with cancellation propagating into them.
9. Profile switches take effect on the next request and announce themselves once.
10. Every rule firing, injection, denial, switch, and gate verdict is emitted as a structured event.
11. Profile identity and counters survive checkpoint/restore; a changed or missing profile fails closed.
12. All rule-engine behaviour is covered by pure unit tests in `harness-core`.
13. Profiles load from `.rusty/profiles/` and from the host registry with the documented precedence; the built-in default cannot be shadowed.
14. A published JSON Schema validates profile documents, and `metadata` round-trips unchanged.
15. Child agents get their behaviour through `ChildBehaviorResolver`; the default inherits the parent's active profile with fresh state.

---

## 14. Decisions

| # | Question | Decision |
|---|---|---|
| 1 | Gate feedback message role | `User` message wrapped in `<system-reminder source="gate">`. No mid-conversation system messages |
| 2 | Per-turn injections vs prompt caching | The system prompt never varies per turn; per-turn text goes at the tail of the request |
| 3 | Where profiles live | Workspace (`.rusty/profiles/*.json`) **and** host registry, plus a **built-in `rusty.default` compiled into the core** (§5.3, §5.4) |
| 4 | Gate `on_exhausted` default | `fail` inside workflow steps (the verify node decides next); `accept` (flagged `gate_passed: false`) in interactive sessions |
| 5 | Child agents | **Inherit the parent's active profile for now**, through a separate, pluggable `ChildBehaviorResolver` and a `children` policy reference with reserved types for future selective inheritance and lifecycles (§8a) |
| 6 | Authoring format | **JSON only**, editable in the UI, with a generated JSON Schema. No markdown format |

Further suggestions will be added here before implementation starts.

---

## 15. Implementation status

### 15.1 Phase 1: profiles (done)

| Piece | Where |
|---|---|
| Profile types, `ProfileRef`, `ChildPolicy` (JSON, `deny_unknown_fields` except `metadata`) | `harness-core/src/behavior/definition.rs` |
| Compiler (all issues in one report, content hash) + helpers used by transitions | `harness-core/src/behavior/compiler.rs` |
| Built-in `rusty.default` (compiled once, shared) | `harness-core/src/behavior/default.rs` |
| `ProfileRegistry`: sources `BuiltIn < Workspace < Host`, reserved `rusty.` prefix, immutable published / editable drafts, resolution order | `harness-core/src/behavior/registry.rs` |
| `BehaviorState` (profile + per-run counters), durable form | `harness-core/src/behavior/state.rs` |
| `ChildBehaviorResolver`, `PolicyChildBehaviorResolver` (default), `InheritParentProfile` | `harness-core/src/behavior/children.rs` |
| Request shaping, tool admission, turn accounting | `harness-core/src/transitions.rs`: `execution_request`, `tool_requested`, `begin_turn`, `set_behavior_profile` |
| `AgentCommand::SetBehaviorProfile` / `SessionCommand::SetBehaviorProfile` | `harness-protocol`, `harness-runtime` |
| Child resolution at spawn | `AgentSupervisor::spawn_child`, replaceable via `SessionRuntime::set_child_behavior_resolver` |
| Persistence + fail-closed restore | `StoredAgentState.behavior`, `projection.rs`, `restorer.rs` (`SessionManagerError::BehaviorRestore`) |
| Engine API | `ProfilesConfig` (`Harness::profiles()`, `HarnessBuilder::profiles`), `SessionBuilder::{profile, workspace_profiles, child_behavior_resolver}`, `SessionHandle::{behavior_profile, set_behavior_profile}` |
| Workspace loading | `harness-engine/src/profiles.rs`: `.rusty/profiles/*.json` (one document or an array per file), `config.json` → `{ "default": ProfileRef }` |
| Orchestration | `AgentNodeConfig.profile`; `IsolatedSessionAgentExecutor::with_profiles`; references checked before a run starts |
| JSON Schema | `schema/behavior-profile-v1.schema.json`, generated from the Rust types; a test fails when it is stale (regenerate with `RUSTY_UPDATE_SCHEMAS=1 cargo test -p harness-core published_json_schema`) |

**Tests: 38 new.**
- Core unit tests (18): compiler, registry, state, children, schema.
- Core state-machine tests (8): `harness-core/tests/behavior_profiles.rs`.
- Runtime (5):
  - restore keeps the profile;
  - an altered snapshot is refused;
  - children inherit the parent's profile by default;
  - a custom resolver chooses the child's profile;
  - a resolver can refuse to create a child.
- Engine end-to-end (7): `harness-engine/tests/behavior_profiles_e2e.rs`.

**Compatibility.** The full pre-existing suite (885 tests) passes unchanged with every agent running under `rusty.default`.

### 15.2 Where the implementation refines this document

- **Snapshots are self-contained.** A snapshot stores the full profile document plus its hash, not just `(id, revision)`. On restore the profile is recompiled and the hash must match, or restore fails with `BehaviorRestore`. Restore therefore doesn't depend on the registry still holding the profile, and tampering or corruption is caught. (§9.1 said "re-resolve through the registry".)
- **`ChildBehaviorResolver::resolve(parent, spec)` has no registry parameter.** A resolver that needs profiles captures its own registry.
- **`SetBehaviorProfile` carries the full, host-validated document.** The core recompiles it. It applies immediately when the agent is idle, otherwise at the start of the next run. Phase 4 changes this to "next request" and adds the announcement.
- **`SessionHandle::set_behavior_profile` already exists**, with the next-run semantics above. §12 had it in phase 4.
- **Denials in phase 1 use the existing `ToolError::PermissionDenied`.** This covers out-of-scope tools, calls over the tool budget, and tools named on the final turn. `ToolError::Denied { reason }` arrives with phase 2 rules.
- **Limits are per run.** Counters reset when a run starts. A model that calls tools after its final turn fails the run with `BEHAVIOR_LIMIT_EXCEEDED`.
- **The final-turn prompt goes on the last tool result** of the outgoing request, or into a text block when the request ends with a user message, and never into the stored transcript. Some providers drop text blocks inside tool messages (§3).
- **Built-in ids are reserved by prefix** (`rusty.`), not just `rusty.default@1`.
- **Profile events** (`ProfileChanged`, …) ship with phase 2's event set; phase 1 adds no `AgentEvent` variants.

### 15.3 Phase 2: rules (done)

| Piece | Where |
|---|---|
| `Rule`, `RuleEvent`, `Condition`, `Action`, `Placement` (JSON) | `harness-core/src/behavior/definition.rs` |
| Pure evaluator and glob matcher | `harness-core/src/behavior/rules.rs`: `BehaviorState::evaluate`, `note_tool_request`, `note_executed` |
| Rule validation, per event (conditions, actions, and placements must fit the event they fire on) | `harness-core/src/behavior/compiler.rs` |
| Run bookkeeping (firings, executed calls, repeat streak, pending injections), persisted | `RunCounters` in `state.rs` |
| Event points | `transitions.rs`: `start_run` (RunStart), `begin_turn` (BeforeModelRequest), `tool_requested` (PreToolUse), `record_tool_result` (PostToolUse / PostToolUseFailure) |
| `ToolError::Denied { reason }` | `harness-protocol`. The model reads `Denied: <reason>` as the result |
| Events | `BehaviorRuleFired`, `ContextInjected`, `ToolCallDenied`, `ProfileChanged` (`AgentEvent`); TypeScript SDK types; `ProtocolCapabilities::behavior_profiles` |

**Tests: 19 new.**
- Core unit tests (9): validation, globs, decision order, `max_fires`, history/argument/loop conditions, snapshot.
- State machine (9): `harness-core/tests/behavior_rules.rs`.
- Engine end-to-end (1): a workspace profile's rule reaches the backend and emits events.

**Compatibility.** Every earlier test passes. The Phase 1 tests were updated for two intended changes: profile switches now emit `ProfileChanged`, and harness refusals now carry a reason.

### 15.4 Where phase 2 refines this document

- **`arg` conditions are an object:** `{"arg": {"pointer", "glob" | "contains" | "equals"}}`. They are not flat, so every condition stays a single-key object (§6.2 updated).
- **Unit-like actions are written as empty objects**, e.g. `{"allow": {}}`, so every action has the same `{name: {...}}` shape.
- **Evaluation order for a tool request:**
  1. session registration
  2. profile scope, which refuses with a reason
  3. rules (`stop_run` stops at once; the first `deny` / `ask` / `allow` decides)
  4. the profile override and session permission, which a rule's `allow` can lift only from `ask`
  5. the tool budget

  A hidden tool never reaches rules.
- **`stop_run` keeps the transcript valid.** On `PreToolUse` the call is first recorded as denied; on post-tool events the result is recorded first. Then the run fails with `BEHAVIOR_STOP`, and the next run starts normally.
- **Every harness refusal now carries a reason** (`Denied: …`, plus a `ToolCallDenied` event): scope, profile `deny` override, budget, final turn, and rules. A user's denial of a permission prompt is still `PermissionDenied`.
- **Post-tool events fire only for calls that executed.** A refused call has no post-tool event; its `PreToolUse` already decided.
- **`next_request` text is consumed when a request is built** and remembered for that turn, so a request re-issued after pause/resume repeats it (§15.7).
- **Replay doesn't rebuild behavior state.** Replaying events recorded after the last snapshot doesn't reconstruct behavior counters. Snapshots carry them (written at run boundaries and on checkpoint), so after a crash mid-run the counters can lag by at most the events since the last snapshot.

### 15.5 Phase 3: completion gate (done)

| Piece | Where |
|---|---|
| `CompletionGate`, `GateCheck`, `EvaluatorSpec` (`tool` / `model`; `agent` reserved), `OnExhausted`, `ErrorPolicy` | `harness-core/src/behavior/definition.rs` |
| Gate validation (exactly one of `require` / `evaluator`, feedback required for `require`, no tool-event conditions) | `harness-core/src/behavior/compiler.rs` |
| Gate state machine | `transitions.rs`: `propose_completion`, `completion_evaluated`, `gate_decided`, and `cancel` (`CancelEvaluation`) |
| Protocol | `AgentStatus::Verifying`, `AgentEffect::{EvaluateCompletion, CancelEvaluation}`, `AgentCommand::CompletionEvaluated`, `CheckVerdict` / `VerdictOutcome`, `AgentEvent::CompletionGateEvaluated`; TypeScript SDK types |
| Evaluators | `harness-runtime/src/completion_gate.rs`; run by `AgentRunner::evaluate_completion` as a cancellable task, with results sent back through the mailbox |
| Workflow steps | `IsolatedSessionAgentExecutor`: an unpassed gate becomes a retryable `completion_gate_not_passed` (`VerificationFailed`) |

**Tests: 17 new.**
- Core unit tests (3): gate validation and defaults.
- State machine (8): `harness-core/tests/behavior_gate.rs`.
- Evaluator parsing (2).
- Runtime end-to-end (3):
  - a tool evaluator keeps the agent working until tests pass;
  - a model evaluator judges the answer;
  - an evaluator that cannot run never passes silently.
- Engine (1): a gated workflow step fails and is retried under its policy.

### 15.6 Where phase 3 refines this document

- **Order of checks.** Declarative `require` checks run first, with no I/O. Evaluators run only if those pass, so a cheap check can save a billed model call.
- **Feedback.** An evaluator's own feedback is used unless the check sets `feedback`, which overrides it. §7.1's `feedback_from` is not needed.
- **`on_exhausted` defaults to `accept` in the core.** The decision to fail inside workflow steps (§14 #4) is made where the context is known: the step executor turns a finished-but-unpassed gate into a retryable failed attempt. The core stays context-free.
- **"Flagged `gate_passed: false`"** is reported two ways: the `CompletionGateEvaluated { passed: false, continuing: false }` event, and `AgentResult::gate_passed` (see §15.7).
- **A rejection on the final turn cannot continue** (no turn is left to address it), so it is handled as exhausted.
- **Pause is a no-op while `Verifying`.** Evaluation is short and bounded. Pausing mid-way would discard the verdict, and resume would re-ask the model. Cancel stops an evaluation.
- **Tool evaluators** act for the harness, so the profile's tool scope doesn't apply: a gate may run a tool the model cannot see. The session's permission does apply. Only `allow` tools run; `ask` or unavailable tools produce an `Error` verdict, which follows the check's `error_policy`.
- **Model evaluators** use the session's backend and the agent's model unless `model` is set.
  - They request structured output when the backend supports it, and the host parses and checks the reply either way.
  - The transcript is rendered as data, capped per entry.
  - Evaluator requests count as the agent's usage (see §15.7).
- **Idiom: "every edit is followed by tests".** Write it as `{"not": {"since_last_call": {"of": "fs.edit", "called": "run_tests", "eq": 0}}}`, which holds when nothing was edited. `since_last_call` is false when `of` never ran; that is the right behavior for rules, but it means `gte 1` alone rejects runs with no edits. The §7.1 example is updated.

### 15.7 Gaps closed after phase 3

- **Evaluator usage.** `CompletionEvaluated` carries the evaluators' model usage. The core adds it to the agent's usage records and emits `UsageUpdated`, so it reaches live state and orchestration budgets. It is counted even when the verdict itself is stale.
- **Gate status in results.** `AgentResult::gate_passed: Option<bool>` is `None` without a gate. A parent sees it through `agent.spawn`'s result as `gate_passed`.
- **Re-issued requests.** A request re-issued after pause/resume repeats the context its turn carried (`RunCounters::last_request`) and does not count as a new turn.
- **Still open:** replaying events recorded after a snapshot does not rebuild behavior counters (§15.4). A pending mid-run switch is not persisted: after a crash, the run resumes under the profile it was using.

### 15.8 Phase 4: profile switching (done)

| Piece | Where |
|---|---|
| `switch_profile` action, `ProfileEntered` event, `profile_entered_from` condition | `definition.rs`, `compiler.rs`, `rules.rs` |
| Switch library: `ProfileRegistry::{resolve_closure, from_library, documents}`; `BehaviorState::{library, entered_from, switched_to, resolve_switch}` | `registry.rs`, `state.rs` |
| `AgentCommand::SetBehaviorProfile { profile, library }`; `SessionCommand::SetBehaviorBundle` | protocol, runtime |
| Switching in the state machine | `transitions.rs`: `set_behavior_profile`, `apply_switch`, `enter_profile`, `begin_turn` |
| Host API | `SessionHandle::set_behavior_profile`, which now applies mid-run before the next request |
| Engine / workflows | switch closure resolved and validated at session start, on host switch, and for workflow steps |

**Tests: 9 new.**
- State machine (5): `harness-core/tests/behavior_switching.rs`:
  - a rule switches plan → build;
  - a host switch mid-run applies at the next request;
  - announcements appear only when there is history;
  - the loop guard stops bouncing between profiles;
  - the library survives a snapshot and children inherit it.
- Unit (2): switch validation; closure resolution with cycles and missing targets.
- Engine (2): a missing switch target fails session start; a host switch is announced to the model.

Plus 5 tests for the §15.7 gaps.

### 15.9 Where phase 4 refines this document

- **The switch library.** The core is pure and has no registry, so a profile is installed together with every profile it can reach through `switch_profile` rules. The host resolves that closure from its registry.
  - An unresolvable target fails session start (or the host switch, or the workflow step) before anything runs.
  - The agent switches only within its library.
  - The library is persisted with the agent and inherited by children.
- **When a switch takes effect.** Rule and host switches apply before the next model request: before the next turn in the same run, or at the start of the next run. A switch requested by a `BeforeModelRequest` rule applies from the following request, since the current one is already being prepared. An idle agent switches immediately.
- **What resets.** Counters start fresh under the new profile: turns, tool calls, `max_fires`, executed-call history, and gate state. Limits and rules therefore count per mode. The run's switch count carries over, and more than `MAX_SWITCHES_PER_RUN` (16) switches in one run fails it with `BEHAVIOR_SWITCH_LOOP`.
- **The announcement.** It is a `next_request` reminder naming both modes and the tools now available. It is sent only when the conversation already has an assistant turn; a fresh conversation has nothing to reinterpret. `ProfileEntered` rules fire on every switch, including the first profile installed on a session (from `rusty.default`). The previous profile's name is captured at switch time, because the new library may not contain it.
- **`profile_entered_from`** matches the previous profile's id (globs allowed) and is only valid on `ProfileEntered`.

### 15.10 Phase 5: rewrites, verifier agents, commands, hook import (done)

| Piece | Where |
|---|---|
| `rewrite_args { merge }` (RFC 7396) and `rewrite_result { replace \| append }` | `definition.rs`, `rules.rs` (`merge_patch`, `ResultRewrite`), `transitions.rs` (`rewrite_arguments`, `record_tool_result`) |
| `agent` evaluator: an isolated verifier session | `harness-runtime/src/completion_gate.rs` (`run_verifier_agent`) |
| `command` evaluator following the `Stop` hook contract | `completion_gate.rs` (`run_command`, `command_verdict`) |
| Command trust | `BehaviorState::commands_trusted`, `SetBehaviorProfile { allow_commands }`, `harness_engine::CommandTrust`, `SessionBuilder::{allow_command_evaluators, trust_workspace_commands}` |
| Hook import | `harness_engine::import_hooks` returning `HooksImport { checks, skipped }`, and `HooksImport::into_gate` |

**Tests.**
- Core: merge patch, rewrite validation, command detection, and two state-machine tests in `harness-core/tests/behavior_rewrites.rs`.
- Runtime:
  - hook-contract exit codes;
  - a trusted command following the `Stop` contract end to end;
  - an untrusted command never runs;
  - an agent evaluator running an isolated verifier.
- Engine: the trust rules; importing a Claude Code settings file into a valid gate.

### 15.11 Where phase 5 refines this document

- **`rewrite_args`.**
  - All matching rules apply, in order, after any `deny`.
  - Conditions of every rule see the original arguments, since rules are evaluated together.
  - The rewritten call replaces the pending call, so an approval prompt shows the arguments that will actually run.
  - The model's own `tool_use` block keeps what it asked for, and a note on the result tells it what ran.
- **`rewrite_result`.** It changes the text stored and shown to the model, in rule order, before any `with_result` context is appended. `replace` is useful for redaction.
- **`agent` evaluator.**
  - It runs in a fresh session with only the tools it names. It cannot ask for approval, so each tool must be allowed outright; otherwise the check errors.
  - It is bounded by `max_turns` (default 8, maximum 50), with a final-turn prompt that demands the verdict.
  - Its usage is reported as one record per request it made, with the aggregate tokens and cost on the first record, so both request counts and totals stay exact.
- **`command` evaluator.**
  - **Invocation:** `sh -c` (`cmd /C` on Windows), in the workspace root, with `RUSTY_PROJECT_DIR` and `CLAUDE_PROJECT_DIR` set.
  - **Input on stdin:** `hook_event_name: "Stop"`, `cwd`, `run_id`, `attempt`, `stop_hook_active` (true after a rejection), `last_assistant_message`, and the last 20 transcript messages rendered as text (instead of Claude Code's `transcript_path`).
  - **Verdict:**
    - exit 0 passes, unless stdout is `{"decision": "block", "reason": ...}`;
    - exit 2 fails with stderr as feedback;
    - anything else, a timeout, or a failure to start is an error, handled by `error_policy`.
- **Command trust.**
  - Commands run arbitrary processes. A profile that uses them is refused at session start (and on host switches and workflow steps) unless the session calls `allow_command_evaluators(true)`.
  - A workspace profile also needs `trust_workspace_commands(true)`, because anyone who can edit the repository can edit it.
  - The host's decision travels with the installed profile (`commands_trusted`): it is persisted, carried across switches, and inherited by children. The runtime refuses to run a command without it.
  - A session restored from storage keeps the trust its agents were installed with; profiles installed later through a restored handle get none unless re-granted.
- **Hook import scope.**
  - Only `Stop` hooks become gate checks:
    - `command` → `command`, with `error_policy: pass`, matching Claude Code, where a failing hook that is not a block does not stop the agent;
    - `prompt` → `model`;
    - `agent` → `agent`.
  - Tool hooks (`PreToolUse`, `PostToolUse`, …) and other hook types are listed in `skipped`. Tool hooks run a script on every tool call and wait for its decision; supporting them would need an asynchronous step in the middle of each call. Declarative rules cover most of what they are used for.
  - Imported commands still need the session's command trust.

### 15.12 Editor support

- `harness_engine::validation` validates raw JSON documents for editors, and never fails. `validate_profile(document, library)` runs the profile compiler and also reports `switch_profile` targets that resolve neither in `library` nor to a built-in. `validate_orchestration(document, profiles)` runs the orchestration compiler and warns about agent steps whose profile is unknown. A parse error comes back as one `parse` issue. Issue paths use the compilers' own notation, such as `rules[2].do` and `nodes.<id>.config`.
- `templates()` returns the built-in profiles and the default workflow, for use as starting points.
- rusty-ide's Behaviors tab uses these functions through the `behavior_validate_profile`, `behavior_validate_workflow` and `behavior_templates` Tauri commands. It edits `.rusty/profiles/*.json` and `.rusty/workflows/*.json`, and stores canvas positions in `metadata.editor.position`.
- Running saved workflows from rusty-ide (Agent Mode):
  - The session recipe carries the workflow document. It is registered on a per-session `OrchestrationConfig` through `register_json`, and the frontend starts it with `harness_start_workflow` instead of sending a prompt.
  - Agent steps share the session's backend and tool registry. Their host model turns and host tool calls therefore arrive on the same bridge, and the same frontend run answers them.
  - The run's orchestration events, the steps' agent events and the final state are forwarded as `workflow_event`, `workflow_agent_event` and `workflow_finished`. A step's permission requests are answered through `harness_workflow_control`.
- `SessionBuilder::allow_draft_profiles` lets draft profiles run. With drafts allowed, a reference without a revision also resolves to the newest draft, and `ProfileRegistry::resolve` follows the same rule. The IDE enables this for workflow sessions only.

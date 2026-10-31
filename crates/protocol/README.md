# rusty-protocol

The shared vocabulary of the whole system: every id, command, event, message and wire type that
crosses a crate or a process boundary. It has no runtime and does no I/O, so anything can depend on
it (the deterministic core, the async runtime, transports, the SDK, the IDE) without taking on
policy it did not ask for.

## Why it exists

A system made of an agent state machine, an async runtime, model providers, tools and several
transports only works if everyone agrees on the words. If those words lived in the runtime, a
provider crate would need the runtime to name a message; if they lived in the core, a client
would need the core to read an event. Putting them in one crate that depends on nothing makes the
dependency arrows point one way and makes the wire contract reviewable in one place: a change to
an event is a change to this crate, and `schema/` plus the replay fixtures show exactly what moved.

## How it works

The crate is data and the small rules that keep the data consistent. The central loop of the
system is expressed entirely in these types:

```text
 AgentCommand ──► Agent::apply (rusty-agent-core) ──► Vec<AgentEffect>
      ▲                                                     │
      └────────── the runtime carries each effect out ◄─────┘
                  and reports the outcome as the next command
```

- A **command** asks an agent to do something (`StartRun`, `Steer`, `FollowUp`, `BackendEvent`,
  `ToolCompleted`, `ToolFailed`, ...).
- An **effect** is what the core wants done in the world (`ExecuteBackend`, `ExecuteTool`,
  `SpawnAgent`, `RequestPermission`, `Persist`, `Emit`, `FinishRun`, `EvaluateCompletion`, and
  the matching cancellations).
- An **event** is what clients observe. Each travels in an `AgentEventEnvelope`.

## Key types and concepts

- **Ids** (`ids`): `SessionId`, `AgentId`, `RunId`, `EventId`, `MessageId`, `ToolCallId`, ...
  Each is a distinct UUID newtype, so passing a run id where an agent id belongs does not
  compile. `Timestamp` wraps a UTC time; `Timestamp::from_sequence` makes deterministic
  timestamps for tests.
- **`AgentEventEnvelope`** (`events`): the event payload plus routing metadata: session, agent,
  parent agent, run, visibility (`User`, `Developer`, `Internal`) and two monotonic counters.
  `agent_sequence` orders one agent's events; `session_sequence` orders the whole session and
  is `None` until the event is committed. Clients order by these counters, never by timestamp,
  because two agents can produce identical timestamps.
- **`AgentStatus`** (`commands`): the agent's state: `Idle`, `PreparingContext`,
  `WaitingForBackend`, `Streaming`, `Executing`, `WaitingForPermission`, `WaitingForChildren`,
  `Verifying`, `Paused`, `Completed`, `Cancelled`, `Failed`.
- **Messages** (`messages`): provider-neutral conversation content: `AgentMessage` made of
  `ContentBlock`s (text, tool use, tool result, image), plus tool-history repair.
- **Backend contract** (`backend`): `ExecutionRequest`, `ExecutionEvent` and `ExecutionResult`,
  the exchange between the agent loop and a model backend, with `ExecutionParams`.
- **Wire contract** (`rpc`): the transport-neutral JSON RPC shared by every transport and SDK,
  currently `PROTOCOL_VERSION` 2.
- **Supporting types**: `tools` (descriptors and execution policy), `usage` (token and cost
  accounting), `mcp` and `skills` (specs), `admission` and `lifecycle` (run admission, lifecycle).

## A worked example

A client wants to follow one agent's output in order:

```rust
use rusty_protocol::events::{AgentEvent, AgentEventEnvelope};

fn render(envelopes: &mut Vec<AgentEventEnvelope>) {
    // Order by the agent's own counter; timestamps can tie.
    envelopes.sort_by_key(|envelope| envelope.agent_sequence);
    for envelope in envelopes.iter() {
        match &envelope.event {
            AgentEvent::AssistantTextDelta { .. } => { /* append to the live message */ }
            AgentEvent::Completed { .. } => { /* the run finished */ }
            _ => {}
        }
    }
}
```

Events delivered after a reconnect carry a `session_sequence`; the client remembers the highest
one it saw and asks the server for everything after it, which is how a stream resumes without gaps
or duplicates.

## Public API

Everything is public by module, because this crate is the vocabulary: `admission`, `backend`,
`commands`, `effects`, `events`, `ids`, `lifecycle`, `mcp`, `messages`, `rpc`, `skills`, `tools`,
`usage`. There are no re-exports at the crate root on purpose: `rusty_protocol::events::...`
names where a type belongs.

## Errors

The crate defines data errors that cross the wire, such as the typed RPC error with stable codes
that clients can match on, and failure types carried inside events. It performs no operation that
can fail at runtime beyond (de)serialization, which reports through `serde_json`.

## Design decisions

- **No behavior.** Anything that decides, schedules or touches the outside world lives in
  another crate. The protocol stays cheap to depend on and safe to share with other languages
  through the JSON schema in `schema/`.
- **Distinct id types.** Newtypes cost a little ceremony and prevent a whole class of mix-ups.
- **Counters over clocks.** Ordering is by sequence numbers so replay and concurrent agents are
  deterministic.
- **Stable on the wire.** Stored sessions and `.rusty/` data must keep loading. A change to an
  existing event or field is a compatibility decision, covered by the replay fixtures in the
  runtime and session-store tests.

## Testing

Round-trip tests sit beside the types (ids, events, messages, rpc), and `tests/ids.rs` checks
the id types from outside the crate. Run `cargo test -p rusty-protocol`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `harness`, `harnessctl`, `harnessd`, `rusty-agent-core`, `rusty-context`, `rusty-context-repo`, `rusty-engine`, `rusty-harness-sdk`, `rusty-model-backend`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-gemini`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-provider-openai-compatible`, `rusty-provider-openai-responses`, `rusty-runtime`, `rusty-session-store`, `rusty-skills`, `rusty-tool-mcp`, `rusty-transport-ipc`, `rusty-transport-mcp`, `rusty-transport-stdio`, `rusty-transport-websocket`, `rusty-workflow-core`, `rusty-workflow-runtime`

## Must never

- Do I/O, spawn tasks, or read the clock for anything but `Timestamp::now`.
- Contain policy: what to do on an event, when to retry, what a tool may touch.
- Depend on any other workspace crate.

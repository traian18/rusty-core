# rusty-tool-api

The contract between an agent and everything it can call. A tool is anything that implements
`ToolExecutor`: the built-in file, shell and git tools, MCP servers, skills, and tools the IDE
provides through the host. This crate defines that contract and the registry that finds tools by
id. It contains no tool implementations.

## Why it exists

The runtime has to execute tools without knowing which tools exist, and every tool crate has to
be buildable without pulling in the runtime. Putting the trait, the data types and the registry in
one small crate with no workspace dependencies gives both sides something to depend on. Adding a
tool is then a new crate (or a registration at startup) and never a change in the runtime.

It is deliberately free of implementations. A placeholder executor that lived here could be
registered in production by mistake; the only executor in the crate is `UnknownTool`, which always
fails and exists to stand in for a configured tool nobody provided.

## How it works

A session builds a `SimpleToolRegistry`, registers one `ToolExecutor` per tool, and hands the
registry to the runtime. From then on:

1. The model is shown `registry.descriptors()`: each tool's id, name, description and JSON input
   schema.
2. When the model asks for a tool, the runtime calls `registry.get_executor(id)`.
3. The runtime scopes the call with `with_tool_call(call_id, session_id, future)` and awaits
   `executor.execute(input, cancel)`.
4. The executor returns a `ToolResult`, or a `ToolError` when something outside the tool's control
   went wrong.

Inside step 3 the executor can read which call and which session it is serving with
`current_tool_call_id()` and `current_tool_session_id()`. Built-in tools never need this. It exists
for forwarding executors, such as the IDE's host tool executor, that hand the work to another
process and must let that process correlate its work with the call's events. Sessions that share
one registry (a workflow step and its parent) would otherwise be indistinguishable to them.

## Key types and concepts

- **`ToolExecutor`**: the trait every tool implements. `descriptor()` says what the tool is,
  `execute()` does the work, and `call_timeout()` lets a tool that legitimately runs long (a build,
  a test suite) or waits for a person ask for a longer limit than the scheduler's default.
- **`ToolDescriptor`**: the model-facing description: `ToolId`, display name, description and the
  JSON Schema of the arguments.
- **`ToolId`**: a string newtype such as `fs.read`. Ids are unique within a registry.
- **`ToolInput`**: the raw JSON arguments. `input.parse::<T>()` deserializes them into the tool's
  own argument struct.
- **`ToolResult`**: the output and an `is_error` flag. The convention is that a problem the model
  can read and correct on its next turn (a bad argument, a missing file) is a `ToolResult` with
  `is_error: true`, not an error.
- **`ToolError`**: infrastructure failures that abort the call: execution failed, permission
  denied, timeout, internal. A bad argument is never a `ToolError`.
- **`ToolRegistry` and `SimpleToolRegistry`**: the lookup contract and its in-memory
  implementation. The trait exists so tests and hosts can supply their own.
- **`CancellationToken`**: re-exported from `tokio-util` so tool crates need no direct dependency
  on it. A tool must stop promptly when it is cancelled.
- **Reporting types** (`ExecutionResult`, `ExecutionFailure`, `FailureKind`, `ToolUsage`,
  `ToolProgress`, `ProgressPhase`): the shapes hosts use to report progress and outcomes.

## A worked example

A tool that echoes its `text` argument:

```rust
use async_trait::async_trait;
use rusty_tool_api::{
    CancellationToken, SimpleToolRegistry, ToolDescriptor, ToolError, ToolExecutor, ToolInput,
    ToolResult,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
struct EchoArguments {
    text: String,
}

struct Echo;

#[async_trait]
impl ToolExecutor for Echo {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "demo.echo".into(),
            name: "Echo".into(),
            description: "Returns the text it is given.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        match input.parse::<EchoArguments>() {
            Ok(arguments) => Ok(ToolResult {
                call_id: String::new(),
                output: json!(arguments.text),
                is_error: false,
            }),
            Err(error) => Ok(ToolResult {
                call_id: String::new(),
                output: json!(format!("bad arguments: {error}")),
                is_error: true,
            }),
        }
    }
}

let registry = SimpleToolRegistry::new();
registry.register_tool(Arc::new(Echo))?;
assert!(registry.get_executor("demo.echo").is_some());
```

The failure case in the example is the convention in action: malformed arguments come back as a
readable error result so the model can retry, while `ToolError` stays reserved for faults the model
cannot fix. Registering `Echo` a second time returns `RegistrationError::DuplicateToolId`.

## Public API

- `ToolExecutor`, `UnknownTool`, `CancellationToken`: the executor contract and its fallback.
- `ToolDescriptor`, `ToolInput`, `ToolResult`, `ToolError`, `ToolId`: the data a call carries.
- `ToolRegistry`, `SimpleToolRegistry`, `RegistrationError`: finding tools by id. The `registry`
  module is public because callers name `rusty_tool_api::registry::ToolRegistry`.
- `with_tool_call`, `with_tool_call_id`, `current_tool_call_id`, `current_tool_session_id`: the
  ambient identity of the call being served.
- `ExecutionResult`, `ExecutionFailure`, `FailureKind`, `ToolUsage`, `ToolProgress`,
  `ProgressPhase`: reporting shapes.

## Errors

- `RegistrationError::DuplicateToolId`: a tool with that id is already registered. The first
  registration is kept. Callers treat it as a configuration bug and surface it at startup.
- `ToolError` is what executors return for infrastructure faults; the runtime turns it into a
  failed tool call event. See the convention above for what is not a `ToolError`.

## Design decisions

- **No implementations here.** Keeps the dependency direction one way: tool crates and the runtime
  depend on this crate, never the reverse, and nothing in it can do I/O.
- **Identity is ambient, not a parameter.** `execute` receives only the arguments so built-in tools
  stay unaware of protocol ids. Task-local scoping lets the few executors that need the ids read
  them without widening every tool's signature.
- **A poisoned registry lock is still used.** The map cannot be left half-updated by a panic in
  another thread, so the registry recovers the lock instead of panicking a second time.
- **The registry trait is synchronous.** Lookups are in-memory; only `execute` is async.

## Testing

Unit tests sit next to the code: `registry/tests.rs` covers lookup, duplicate registration and a
poisoned lock; `call_context.rs` covers scoping of call and session ids. Run them with
`cargo test -p rusty-tool-api`. Tool crates test their own executors against this contract.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `rusty-engine`, `rusty-extension-api`, `rusty-runtime`, `rusty-skills`, `rusty-tool-code`, `rusty-tool-filesystem`, `rusty-tool-git`, `rusty-tool-mcp`, `rusty-tool-repo`, `rusty-tool-shell`, `rusty-tool-web`, `rusty-workflow-runtime`

## Must never

- Contain a real tool implementation or perform I/O.
- Depend on another workspace crate.
- Interpret a tool's arguments or output; that belongs to the tool and to the model.

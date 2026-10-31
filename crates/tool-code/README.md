# rusty-tool-code

The call-graph tools: `code_graph.refresh`, `code_graph.calls_from` and `code_graph.calls_to`,
over `rusty-code-analysis`. They answer "what does this function call" and "what calls this
function" across eleven languages, with each edge marked resolved, possible or unresolved.

## Why it exists

Understanding the effect of a change often comes down to the call graph. Analysis is expensive,
so it is done once into an immutable snapshot (`refresh`) and then queried cheaply. This crate is
only the agent-facing surface; the analysis lives in `rusty-code-analysis`.

## How it works

`code_graph.refresh` analyses the workspace files and publishes a snapshot, returning its id,
coverage, limits and statistics. It must be called before the query tools. `calls_from` and
`calls_to` take a snapshot id and a symbol and return edges, each with a resolution: `Resolved`,
`Possible` (several candidates) or `Unresolved`. Responses share one versioned envelope
(`SCHEMA_VERSION`).

## Key types and concepts

- **`tool` and `tools`**: build one executor (by tool name) or all of them over a shared `Arc<dyn CallGraph>`.
- **Snapshot**: an immutable result of an analysis; queries name the snapshot they read.
- **Resolution**: how sure an edge is, so the model does not treat a guess as fact.
- **Languages**: JavaScript, TypeScript, Python, Java, Kotlin, C#, Go, Rust, PHP, C++, Ruby.

## A worked example

```json
{ "tool": "code_graph.refresh", "arguments": {} }
{ "tool": "code_graph.calls_to", "arguments": { "snapshot": "snap-1f3a", "symbol": "parse_config" } }
```

lists every call site of `parse_config` with its file and line, marking calls through a trait or
dynamic dispatch as `Possible`.

## Public API

- `tools(graph)` and `tool(graph, name)`, building the executors over a `CallGraph`.

## Errors

`CallGraphError` (unknown snapshot, unknown symbol, analysis limit reached, unsupported
language) is returned as an error result with a stable code, so the model can refresh and retry.

## Design decisions

- **Refresh is explicit.** Queries never silently re-analyse; they read the snapshot named.
- **Resolution is part of every edge.** Uncertainty is data, not hidden.

## Testing

`tests/tool_contract.rs` checks the tools' descriptors and responses against the call graph
service. Run `cargo test -p rusty-tool-code`.

## In the workspace

- **Depends on:** `rusty-code-analysis`, `rusty-tool-api`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.

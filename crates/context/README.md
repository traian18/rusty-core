# rusty-context

Context assembly and compaction. Before each model request, a `ContextProvider` may rewrite the
outgoing system prompt and messages: inject project instructions, add workspace orientation, and keep
the conversation inside the model's window. Canonical history is never changed; only the view sent to
the backend is.

## Why it exists

A long session outgrows any model's window, and cutting history at the wrong place breaks the
conversation (a tool result without its call is invalid). This crate makes the cut safely and in
tiers, cheapest first, so most requests are never summarized at all. It stays apart from the
runtime because compaction is a view over history, not part of running an agent.

## How it works

1. `ContextAssemblingBackend` wraps an `ExecutionBackend` so that every request passes through a
   provider chain before it reaches the model.
2. Providers are small: `StaticSystemPromptProvider` and `WorkspaceInfoProvider` add to the system
   prompt; `TruncatingCompactionProvider` drops old turns by characters;
   `PolicyDrivenCompactionProvider` sizes the cut from the model's real window using a
   `ContextPolicy`; `ChainedContextProvider` applies several in order.
3. The pure tiers live in `compaction`: elide bulky old tool output, then `drop_prefix`, which keeps
   the recent turns and every protected message and closes the kept set over tool pairs so neither
   half is orphaned.
4. `SummarizingCompactionProvider` never blocks a request: under soft pressure it starts a
   background task that asks an optional `ImportanceJudge` which old messages matter, summarizes
   the rest with a `Summarizer`, folds in the previous summary, and applies the result on a later
   request.
5. `SupersededReadsProvider` replaces a file read that a later read of the same file has made
   stale.

## Key types and concepts

- **`ContextProvider`**: rewrites an `ExecutionRequest`; providers chain.
- **`ContextPolicy`**: soft, hard and target pressure against the window, and the
  `ContextDecision` each request gets (proceed, schedule background compaction, compact now,
  unavailable).
- **`Retention`**: pinned messages and the opening request, which survive every cut.
- **`Summarizer` / `BackendSummarizer`**: condenses messages into a running summary.
- **`ImportanceJudge`**: rates old messages essential, useful or disposable. The JEV judge is
  optional; without one nothing leaves the process.

## A worked example

```rust
let chain = ChainedContextProvider::new(vec![
    Arc::new(StaticSystemPromptProvider::new("Follow the project's conventions.")),
    Arc::new(PolicyDrivenCompactionProvider::new(inner, policy, Some(window), 8, 120_000)),
]);
let backend = ContextAssemblingBackend::new(model_backend, Arc::new(chain), workspace);
```

## Public API

- `ContextProvider`, `ContextAssemblingBackend`, `ContextPolicy` and its budget types.
- Providers: `StaticSystemPromptProvider`, `WorkspaceInfoProvider`, `TruncatingCompactionProvider`,
  `PolicyDrivenCompactionProvider` (with `last_compaction` and `compaction_count`),
  `ChainedContextProvider`, `SummarizingCompactionProvider`, `SupersededReadsProvider`.
- `Summarizer`, `BackendSummarizer`, `ImportanceJudge`, `JevImportanceJudge`
  (`OpenRouterDecisions` behind the `jev-http` feature).
- `compaction::{drop_prefix, elide_tool_results, truncation_note}`.

## Errors

Providers do not fail a request: a summarizer or judge error is logged and the cheaper tiers keep the
request in budget. `SummarizeError` and `JudgeError` describe those failures; `ContextPolicyError`
describes an invalid policy.

## Design decisions

- **Tiered, cheapest first.** Elide tool output, then drop old turns, and only then summarize.
- **Never block on a model call.** A summary is applied once ready; until then the cheap tiers
  protect the request.
- **Tool pairs are atomic.** The cut point moves earlier rather than orphaning a call or a result.
- **Poison-tolerant locks** for the provider state, shared with the runtime.

## Testing

Each provider and the pure compaction tiers have unit tests, including tool-pair closure and the
background summary flow with fake summarizers and judges. Run `cargo test -p rusty-context`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `rusty-context-repo`, `rusty-engine`, `rusty-skills`

## Must never

- Change the canonical conversation history.
- Block a request on a summary.
- Split a tool call from its result.
- Send message text to a judge or summarizer the host did not configure.

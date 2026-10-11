# harness-context

Context assembly and compaction. Before each model request, a `ContextProvider` may rewrite the outgoing `system_prompt` and `messages`: inject project instructions, add workspace orientation, and keep the conversation inside the model's window. Canonical history is never changed; only the view sent to the backend is.

## What's inside

- `provider` / `providers` — the `ContextProvider` trait and the basic providers: `StaticSystemPromptProvider`, `WorkspaceInfoProvider`, `ChainedContextProvider`, and the budget-driven `TruncatingCompactionProvider` / `PolicyDrivenCompactionProvider`.
- `policy` — `ContextPolicy`: soft/hard/target pressure thresholds against the model's real window, and the decision each request gets.
- `compaction` — the pure tiers: elide old tool output, cut only at tool-pair-safe boundaries (a tool call always stays with its result), keep pinned messages and the opening request.
- `summary` — `SummarizingCompactionProvider`: builds an incremental model-written summary in the background (never blocking a request), one per conversation, applied on a later request; `BackendSummarizer` runs it through any `ExecutionBackend`.
- `importance` — the pluggable `ImportanceJudge` that rates how much an old message still matters (essential / useful / disposable). `JevImportanceJudge` does it with the JEV Decisions API; the `jev-http` feature adds an OpenRouter transport.
- `backend` — `ContextAssemblingBackend`, which wraps an `ExecutionBackend` so every request passes through the provider chain.

Compaction is tiered, cheapest first: elide tool output, drop the oldest turns, and, when enabled, replace aged-out history with a summary. JEV is optional; without a judge nothing leaves the process.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-runtime`
- **Used by:** `harness-engine`, `harness-skills`

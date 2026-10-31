# rusty-tool-repo

Agent-facing tools over the repository knowledge: structure, imports and impact
(`repo_graph.*`), git history signals (`repo_git.*`), ranked findings (`repo_findings.*`), the
one-call `repo_context` orientation tool, and `repo_wiki`. Each toolset is opt-in and appears only
when the thing it needs was supplied.

## Why it exists

`rusty-repo-model`, `rusty-git-analysis` and `rusty-findings` answer detailed questions, but an
agent works through tools and a limited context. This crate shapes those answers: a stable
envelope, size budgets, paging, and an orientation tool that answers "where am I and what
matters" in one call instead of twenty. It is one of the two crates allowed to see both the
engine's tool contract and the repository layer.

## How it works

`RepositoryIntelligence` holds the optional parts: a `RepositoryGraph`, a `GitAnalysis` and a
`FindingsAnalysis`. Building the tools skips any toolset whose part is missing, so the factory
fails closed. Every tool returns the same envelope (`schema_version`, snapshot ids, component
versions, `partial`, `truncation`, `result`) or a structured error with a stable code.

`repo_context` (`context/`) answers in one call. A request names a `target` (or `targets` for
changes) and a mode: `connections` (the default: what the file or folder imports and what
imports it), `orient` (what it is for and what matters), `changes` (the effect of a set of
changed files) or `blast_radius` (what a change could break). Each answer is trimmed to a budget
(`budget`).

## Key types and concepts

- **`RepositoryIntelligence`**: the shared, optional capabilities for one authorized repository.
- **Tool envelope**: the same top-level fields for every tool, so a model learns one shape.
- **`repo_context` modes**: `connections`, `orient`, `changes` and `blast_radius`.
- **Budgets and truncation**: every answer states what it left out.

## A worked example

```json
{ "tool": "repo_context", "arguments": { "target": "src/runtime/", "mode": "orient" } }
```

returns what the folder is for, its main files, what depends on it, recent churn and the next
calls worth making, in one bounded answer; `repo_graph.dependents` then lists the importers of one
file, page by page.

## Public API

- `RepositoryIntelligence` and its constructors, the tool factory functions, and `SCHEMA_VERSION`.

## Errors

Failures are structured errors with stable codes (for example an unknown snapshot, a stale
cursor, or a missing analysis) rather than free text, so a model can branch on them. Bounded
answers are `partial` with a `truncation` description, not errors.

## Design decisions

- **Opt-in toolsets that fail closed.** A tool exists only if its capability does.
- **One envelope.** Learning one response shape is cheaper than learning twenty.
- **Honest truncation.** What was cut is always stated.
- **Known debt.** A few `repo_context` answer builders are long single functions
  (`changes::answer`, `connections::file`, `blast::answer`, `orient::folder`); they are linear
  but should be cut into steps when this crate is next touched.

## Testing

`tests/context_tool.rs` and `tests/context_real_repository.rs` exercise `repo_context` against
fixture and real repositories, and unit tests beside each toolset check the envelope contract. Run
`cargo test -p rusty-tool-repo`.

## In the workspace

- **Depends on:** `rusty-findings`, `rusty-git-analysis`, `rusty-repo-model`, `rusty-repo-wiki`, `rusty-tool-api`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
- Return a repository answer without saying whether it is partial.

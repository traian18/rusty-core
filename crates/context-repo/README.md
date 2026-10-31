# rusty-context-repo

A `ContextProvider` that gives a model the repository documentation it most likely needs. Each
request gets a stable overview of the repository and, when the prompt or the work in progress names
something real, excerpts of the pages about it with the reason each was chosen.

## Why it exists

A model working in a repository should not have to search for orientation, and it should not be
flooded either. This provider turns the wiki's planner into a bounded, labelled block added to the
request, rebuilt from published snapshots every time and never stored in history, so it cannot go
stale or grow the conversation.

## How it works

1. On each request the provider reads the current repository snapshot (and Git and findings sets
   when attached) and the focus (open file, touched files, hand-off files, dirty files).
2. `ContextPlanner` chooses pages; the provider renders the overview plus excerpts for the chosen
   ones, related pages as one-line pointers, within a character budget.
3. With nothing to anchor on, only the overview is sent. The text is labelled as data.
4. An `InjectionReport` records what was injected and why, for display.

## Key types and concepts

- **`RepositoryContextProvider`**: the provider, built with `new(graph)` and `with_*` options.
- **`FocusSource`**: what the user is working on.
- **`InjectionReport` / `InjectedPage`**: what was added and the reason.

## A worked example

```rust
let provider = RepositoryContextProvider::new(graph)
    .with_git(git)
    .with_findings(findings)
    .with_focus(focus)
    .with_task(TaskKind::Debug);
```

## Public API

- `RepositoryContextProvider`, `RepositoryContextConfig`, `InjectionReport`, `InjectedPage`.

## Errors

None are returned: a missing snapshot means the provider adds nothing.

## Design decisions

- **Read-only.** It never scans, refreshes, calls a model or waits for a refresh.
- **Per-request, not stored.** The block is rebuilt each time so it matches the current files.

## Testing

Tests inject into fixture requests and check the budget, the labels and the report. Run
`cargo test -p rusty-context-repo`.

## In the workspace

- **Depends on:** `rusty-context`, `rusty-findings`, `rusty-git-analysis`, `rusty-protocol`, `rusty-repo-model`, `rusty-repo-wiki`, `rusty-runtime`
- **Used by:** nothing else in the workspace

## Must never

- Scan the repository or wait for a refresh.
- Write the injected text into history.
- Present repository text as instructions.

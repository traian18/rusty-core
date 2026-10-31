# rusty-git-analysis

Git intelligence: churn, co-change and recent-contributor evidence from local history. Results
describe historical correlation inside an analysed window, never causation, dependency or ownership.

## Why it exists

The static import graph cannot see that two files always change together, or that one file is
rewritten every week. History can. It is a separate domain with its own snapshots so that a slow
history read never blocks the import graph, and so no consumer mistakes "changed together" for "depends
on".

## How it works

1. `GitHistorySource` (libgit2 in `source`) yields commits and their changed paths; `history` reads
   a window and extends it incrementally.
2. `aggregate` follows renames and computes churn, co-change pairs and contributors.
3. `fix_rule` marks bug-fix commits; `signals` derives per-file signals: recent activity, decayed
   churn, hotspots, change entropy and ownership.
4. Results become an immutable snapshot (`snapshot`) kept by a `GitSnapshotRepository` with retention
   (`store`), and are read through paged `queries`.
5. `service` wires it together as the production `GitAnalysis`.

## Key types and concepts

- **Window**: the commits analysed, bounded by count and age.
- **Snapshot**: an immutable result tied to a history head.
- **Signal**: a windowed per-file measurement, with the evidence it came from.

## A worked example

```rust
let analysis = GitAnalysisBuilder::for_repository(root).build()?;
analysis.refresh(&cancel).await?;
let churn = analysis.churn(&ChurnQuery::default())?;      // paged, against the current snapshot
let pairs = analysis.co_changed(&CoChangeQuery::default())?;
```

## Public API

- `GitHistorySource`, `GitAnalysis`, `GitSnapshotRepository` (the contracts).
- The builder and the production service; the model, config and error types.

## Errors

`GitAnalysisError` covers a missing repository, an unreadable history, a cancelled refresh and an
unknown snapshot. A shallow clone yields a truncated, labelled window rather than an error.

## Design decisions

- **Correlation is labelled as such**, so downstream text never claims ownership or dependency.
- **Incremental windows.** A refresh extends the previous read instead of starting over.

## Testing

Tests build small repositories with libgit2 and check renames, merges, windows and signals. Run
`cargo test -p rusty-git-analysis`.

## In the workspace

- **Depends on:** `rusty-repo-model`
- **Used by:** `rusty-code-health`, `rusty-context-repo`, `rusty-findings`, `rusty-infra-analysis`, `rusty-repo-wiki`, `rusty-tool-repo`

## Must never

- Shell out to `git`: it reads through libgit2.
- Present co-change as dependency or churn as quality.
- Write to the repository.

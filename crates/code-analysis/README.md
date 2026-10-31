# rusty-code-analysis

Call graphs and source metrics from tree-sitter, for ten languages. It answers "who calls this
function" and "how complex is this function" without compiling or running anything, and it is the
measuring instrument behind code health, the guidelines checks and the repository tools.

## Why it exists

Two questions need the same parsed syntax: where do calls go, and how big or tangled is each
function. Parsing once per file and sharing the result is cheaper and keeps the answers consistent.
The call graph is deliberately conservative: a call it cannot place is reported as unresolved with a
rule and an explanation, not guessed.

## How it works

1. `AnalysisSource` lists and reads source files (`FileSystemSource`, with a unix path that opens
   files safely).
2. A `LanguageAdapter` (one per language under `adapters/lang`) walks a file and returns an
   `ExtractedFile`: callables, types, imports, exports and call sites as facts.
3. `CallGraphBuilder` collects those facts into a `ProjectIndex`; a `LanguageResolver` resolves each
   call to targets or an `Outcome` that says why not (`adapters/resolve` implements the shared
   lexical, member, receiver and inheritance rules; each language supplies a `Scheme`).
4. `DefaultCallGraph` turns the resolved calls into a `GraphSnapshot` and answers queries (callers,
   callees, traversals) with bounds; `SnapshotRepository` keeps snapshots.
5. `metrics` measures a file separately: per-function size, cyclomatic complexity, nesting,
   parameters, cohesion and error-handling smells, by language dialect.

## Key types and concepts

- **Fact**: a syntactic observation (`CallFact`, `CallableFact`, `TypeFact`, ...) with a span.
- **`Scheme`**: the per-language rules for imports, namespaces and receivers.
- **`Outcome`**: resolved targets, or unresolved with the rule and explanation.
- **`GraphSnapshot`**: an immutable call graph queried by path or symbol.
- **Dialect**: the per-language table of node kinds metrics count.

## A worked example

```rust
let graph = CallGraphBuilder::new(root).build()?; // then refresh it and query
let callers = graph.calls_to(query).await?;       // who calls a callable
let metrics = rusty_code_analysis::metrics::measure_file(&LanguageId::Rust, path, source)?;
```

## Public API

- `CallGraph`, `CallGraphBuilder`, `DefaultCallGraph`, `GraphSnapshot`, `SnapshotRepository`.
- `LanguageAdapter`, `LanguageResolver`, the fact types and `Outcome`.
- `AnalysisSource`, `FileSystemSource`.
- `metrics::{measure_file, ...}` and the smell detectors.

## Errors

`CallGraphError` covers an unknown symbol, a snapshot that is not available and source failures.
Unparsable files are skipped with a diagnostic. Unresolvable calls are `Outcome`s, not errors.

## Design decisions

- **Conservative by construction.** An unresolved call is more useful than a wrong edge.
- **Shared engine, per-language scheme.** The lookup rules are written once; a language states only
  what differs.
- **Unsafe is documented.** The few unsafe blocks carry a safety comment, enforced by the
  `undocumented_unsafe_blocks` lint.

## Testing

Fixture tests per language adapter and per metric, plus calibration tests for the smell detectors.
Run `cargo test -p rusty-code-analysis`; a stale test binary after moving fixtures is fixed by
touching `tests/metrics.rs`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `rusty-code-health`, `rusty-engine`, `rusty-guidelines`, `rusty-infra-analysis`, `rusty-repo-wiki`, `rusty-tool-code`

## Must never

- Compile or execute the code it measures.
- Report a call target it could not prove.
- Depend on the engine or on a model.

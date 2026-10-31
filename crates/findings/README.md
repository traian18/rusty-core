# rusty-findings

Ranked, explainable observations derived from the repository graph, dependency groups and Git
analysis: cycles, undeclared dependencies, boundary violations and the like. A finding carries its
rule id and version, severity, confidence, evidence, locations and a rationale.

## Why it exists

Raw graph facts are not what a developer or a model needs; they need "this is a problem, here is
why". Findings are derived data kept apart from graph entities so rules can change version without
touching the graph. There are no health scores: findings rank by kind, severity, confidence and then
impact, so the order is explainable.

## How it works

1. `view` indexes one repository snapshot for read-only lookups.
2. `rules` holds one versioned rule per finding type; `analyzer` runs them, bounds the output,
   applies suppressions and boundary policy (`config`), resolves byte spans to lines (`source`) and
   ranks.
3. `set` is the immutable result with paged queries; `service` is the consumer API.

## Key types and concepts

- **Rule**: a versioned detector with a stable id.
- **Evidence**: the graph facts a finding rests on.
- **Suppression / boundary policy**: user configuration that silences or defines violations.

## A worked example

```rust
let analyzer = DefaultFindingsAnalyzer::standard(FindingsConfig::default())?;
let set = analyzer.analyze(AnalysisInput { /* snapshot, optional Git facts */ })?;
// the set is immutable and answers paged queries by kind, severity and path
```

## Public API

- `FindingsAnalyzer`, `DefaultFindingsAnalyzer`, `AnalysisInput`, `FindingsConfig`, `BoundaryPolicy`,
  `Suppression`, the finding model, `FindingSet` and the service.

## Errors

`FindingsError` covers an invalid configuration and a source that cannot be read. A rule that
cannot run yields a labelled gap, not a silent absence.

## Design decisions

- **Measurable versus candidate.** Facts are findings; heuristics are labelled candidates.
- **Stable ids.** A finding's id is computed from its rule and evidence so it survives refreshes.

## Testing

Each rule has fixture-graph tests; ranking and suppression have their own; a benchmark guards
analysis time. Run `cargo test -p rusty-findings`.

## In the workspace

- **Depends on:** `rusty-git-analysis`, `rusty-repo-model`
- **Used by:** `rusty-context-repo`, `rusty-repo-wiki`, `rusty-tool-repo`

## Must never

- Invent a score for a repository.
- Report a candidate as a fact.
- Modify the graph it reads.

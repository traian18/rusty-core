# rusty-code-health

Code health: deterministic detectors over a file's measurements. Each finding has a severity, a
category, the numbers it was judged on and a reason a developer can read. No model is used.

## Why it exists

"Is this code getting worse" needs numbers a reviewer can check. The detectors only say what they
observed; scoring is a separate step that turns findings into defect and maintainability scores, and
rollup turns those into bands for folders and the repository. Keeping the steps apart lets the
thresholds change without changing what was observed.

## How it works

1. `context` gives a detector everything known about one file (functions, metrics, history).
2. Each detector under `detectors` (large method, complex method, nested complexity, bumpy road,
   brain method, god class, low cohesion, duplicated code, error handling, tests coverage,
   primitive obsession, complex conditional, and history-based ones) returns findings, using
   thresholds from `config`.
3. `duplication` finds repeated blocks across files; `trend` tracks maintainability over time.
4. `scoring` turns findings into scores; `rollup` produces bands, repository and folder numbers.

## Key types and concepts

- **`Detector`**: a pure function from a file context to findings.
- **`Finding`**: severity, category, the measured numbers and the reason.
- **Band**: a named range of a score used in rollups.

## A worked example

```rust
let findings: Vec<Finding> = detectors::registry()
    .iter()
    .flat_map(|d| d.detect(&context, &DetectorConfig::default()))
    .collect();
// `scoring` then turns the findings into scores, and `rollup` into bands
```

## Public API

- `Detector`, `detectors` (registry), `Finding`, `Severity`, `Category`, the config types,
  `scoring`, `rollup`, `duplication`, `trend`.

## Errors

Detectors do not fail: a file with no measurements simply has no findings. Configuration is
validated when it is built.

## Design decisions

- **Observation, scoring and rollup are separate steps.**
- **Reasons are text a developer can act on**, with the numbers included.

## Testing

Each detector has threshold tests; the scoring calibration is checked against known repositories.
Run `cargo test -p rusty-code-health`.

## In the workspace

- **Depends on:** `rusty-code-analysis`, `rusty-git-analysis`
- **Used by:** `rusty-guidelines`, `rusty-infra-analysis`, `rusty-repo-wiki`

## Must never

- Call a model or the network.
- Judge a file without showing the numbers.
- Depend on the engine.

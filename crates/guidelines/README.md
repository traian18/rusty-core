# rusty-guidelines

Code guidelines: a project's own style and rules (`.rusty/guidelines/rules.json` and `style.md`),
checked against files as they are edited, next to code health. Rules can be regex or tree-sitter
patterns, size and complexity limits, or import boundaries.

## Why it exists

A project's conventions are usually tribal. Writing them as data lets the tools check them the
moment a file changes, and lets the prose half of the guide go to the model as instructions. Keeping
the checker free of I/O and models means the same rules run in the IDE, the CLI and goal loops.

## How it works

1. `config` is the rules file as written; `compile` validates it into `Guidelines`.
2. `check_file` runs one file through every rule that applies (`glob` chooses files, `pattern` runs
   regex and tree-sitter rules, `metric` the size and complexity limits, `boundary` forbidden
   imports) and returns `Violation`s.
3. `changed` keeps only violations a change introduced.
4. `style` splits `style.md` into sections by the files they apply to, for the model.
5. The JSON schema of `rules.json` is published from the types (`schema/guidelines-v1.schema.json`).

## Key types and concepts

- **`Rule` / `RuleKind`**: pattern, limit, boundary, or prose-only.
- **`Violation`**: rule, file, lines, message and severity.
- **`Kinds`**: which kinds of rule a check run includes.
- **Limits rule**: one rule id for all size and complexity limits.

## A worked example

```rust
let guidelines = Guidelines::parse(Some(rules_json), Some(style_md))?;
let violations = check_file(&guidelines, &FileInput { path, source, metrics, imports: None }, Kinds::ALL);
```

## Public API

- `Guidelines`, `check_file`, `FileInput`, `Violation`, `Kinds`, `retain_introduced`, `StyleGuide`,
  `GuidelinesFile` and `schema()`.
- The example `report` prints a repository's debt per crate.

## Errors

`ConfigError` names a rule that does not compile (bad regex, bad query, bad glob).

## Design decisions

- **Prose rules are never judged here**; they are handed to the model as instructions.
- **Same engine everywhere**; hosts read files and resolve imports.

## Testing

Rule kinds, globs, limits and changed-line filtering each have tests; the schema is regenerated with
`RUSTY_UPDATE_SCHEMAS=1`. Run `cargo test -p rusty-guidelines`.

## In the workspace

- **Depends on:** `rusty-code-analysis`, `rusty-code-health`
- **Used by:** nothing else in the workspace

## Must never

- Read files or run a model.
- Judge a prose rule.
- Report a violation the change did not introduce when asked for only introduced ones.

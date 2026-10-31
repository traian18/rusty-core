# rusty-infra-analysis

Infrastructure analysis: deterministic rules over Terraform, Terragrunt, CI pipelines, Dockerfiles,
Kubernetes manifests and the other files a repository runs on. Rules report the same `Finding`s as
code health, so scoring and the Code Health page treat them alike.

## Why it exists

Much of what breaks in production is not in functions at all. Infrastructure files have their own
syntax and their own mistakes, and relate across files (a Terragrunt unit points at a Terraform
module). One crate holds those rules so the code-health pipeline can include them without learning
each format.

## How it works

1. `kind` decides which kind of infrastructure file a path is; `file` parses it for that kind into
   a model with line numbers (`doc`: HCL, YAML, Dockerfile).
2. `rule` defines `InfraRule`; `rules` holds one module per pack and `check_file` runs them.
3. `graph` relates Terraform modules and Terragrunt units across files.
4. `history` adds Git-history findings for files without functions; `scoring` is a scoring
   configuration that includes these rules.

A finding is suppressed with `# rusty-ignore: rule_id` on its line or the line above.

## Key types and concepts

- **`InfraFile`**: one parsed file with line numbers.
- **`InfraGraph`**: modules and units related across files.
- **`InfraRule`**: a deterministic check producing `Finding`s.

## A worked example

```rust
let file = InfraFile::new(path, &text).expect("a path that is an infrastructure file");
let findings = rules::check_file(&file, &context, &InfraConfig::default());
```

## Public API

- `InfraFile`, `InfraGraph`, `InfraOverview`, `history_findings`, the `rules` registry and the
  scoring configuration.

## Errors

An unparsable file yields a finding that says so rather than an error, so one bad file does not
hide the rest.

## Design decisions

- **Same finding type as code health**, so there is one scoring and display path.
- **Line-level suppression** keeps exceptions next to the code they excuse.

## Testing

Each rule pack has fixtures with a passing and a failing file. Run `cargo test -p rusty-infra-analysis`.

## In the workspace

- **Depends on:** `rusty-code-analysis`, `rusty-code-health`, `rusty-git-analysis`
- **Used by:** `rusty-repo-wiki`

## Must never

- Run Terraform, Docker or kubectl.
- Contact a cloud provider.
- Report a finding without a line to point at.

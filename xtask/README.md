# xtask

Repository maintenance tasks, kept outside the workspace (`exclude = ["xtask"]`) so it never ships in a build. They keep the structure the rewrite established from drifting back.

## Why it exists

A convention that is only written down erodes. These checks turn the three structural rules of the workspace into commands that fail: the pure core stays pure, crates depend on each other in one direction, and every crate README is present and true.

## How it works

Each command reads the workspace through `cargo metadata` (`workspace.rs`), produces a list of findings (a package and a problem), and `report.rs` prints them grouped by package. Any finding makes the command fail; `--warn` prints them and succeeds, for looking at the debt while working on it.

| Command | Checks |
|---|---|
| `check-deps` | `rusty-agent-core`, `rusty-workflow-core` and `rusty-protocol` reach no I/O: no HTTP, database, UI, transport or provider crate (`reqwest`, `rusqlite`, `ratatui`, `tungstenite`, `rmcp`, …) and no `rusty-skills`, which walks the filesystem |
| `check-layers` | Crates are in layers (engine, intelligence, product, bridge, assembly) and may depend only on their own layer or a lower one; a new crate must be given a layer |
| `check-readmes` | Every crate and app has a README with *Why it exists*, *How it works*, *Public API*, *In the workspace* and *Must never*, at least 40 lines, and `Depends on` / `Used by` lines that match `Cargo.toml` |
| `check-all` | All three |

## Public API

A binary, run from the repository root:

```sh
cargo run --manifest-path xtask/Cargo.toml -- check-all
cargo run --manifest-path xtask/Cargo.toml -- check-readmes --warn
```

## Errors

Exit status 1 for any finding, an unknown command, or a workspace that cannot be read.

## Design decisions

- **Outside the workspace**, so building the product never builds the checks.
- **Metadata, not text search.** Dependencies come from `cargo metadata`, so renames and feature-gated dependencies are seen as cargo sees them.
- **The README check compares dependency lists**, so a README cannot claim a dependency that does not exist.

## In the workspace

- **Depends on:** nothing in the workspace; it reads the workspace through `cargo metadata`.
- **Used by:** CI and anyone changing crate structure.

## Must never

- Be a dependency of a product crate.
- Pass a finding silently: use `--warn` to look, not to hide.

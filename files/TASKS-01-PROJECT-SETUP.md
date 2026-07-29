# Phase 0 — Project Setup

**Goal:** a blank repo becomes a buildable Cargo workspace containing every crate/app named in spec Section 65, with CI, licensing, and lint/format conventions in place — before any domain logic is written.
**Depends on:** nothing (first phase).
**Crates touched:** all (created empty/stub in this phase).

Full crate → path → package-name mapping used for the rest of this project (do not deviate without updating this table):

| Path | Package name |
|---|---|
| `crates/harness-protocol` | `harness-protocol` |
| `crates/harness-core` | `harness-core` |
| `crates/harness-runtime` | `harness-runtime` |
| `crates/harness-engine` | `harness-engine` |
| `crates/harness-model` | `harness-model` |
| `crates/harness-generic-backend` | `harness-generic-backend` |
| `crates/harness-tools` | `harness-tools` |
| `crates/harness-context` | `harness-context` |
| `crates/harness-workspace` | `harness-workspace` |
| `crates/harness-session-store` | `harness-session-store` |
| `crates/harness-extension-api` | `harness-extension-api` |
| `crates/integrations/anthropic` | `harness-integration-anthropic` |
| `crates/integrations/openai` | `harness-integration-openai` |
| `crates/integrations/gemini` | `harness-integration-gemini` |
| `crates/integrations/openai-compatible` | `harness-integration-openai-compatible` |
| `crates/integrations/claude-code` | `harness-integration-claude-code` |
| `crates/integrations/codex` | `harness-integration-codex` |
| `crates/tools/filesystem` | `harness-tool-filesystem` |
| `crates/tools/shell` | `harness-tool-shell` |
| `crates/tools/git` | `harness-tool-git` |
| `crates/transports/websocket` | `harness-transport-websocket` |
| `crates/transports/stdio` | `harness-transport-stdio` |
| `crates/transports/ipc` | `harness-transport-ipc` |
| `apps/harness` | `harness` (standalone TUI bin) |
| `apps/harnessd` | `harnessd` (daemon bin) |
| `apps/harnessctl` | `harnessctl` (control CLI bin) |

26 workspace members total.

---

## Tasks

### Task 0.1 — Repo baseline files
- **Files:** `.gitignore`, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`
- **Description:** Standard Rust `.gitignore` (`/target`, editor/OS cruft). A top-level `README.md` describing the project per spec Section 1 (executive summary) and linking to `rust-agent-harness-development-spec.md` and `TASKS-00-OVERVIEW.md`. Add both license texts; dual-license as `MIT OR Apache-2.0` (ecosystem-standard for reusable Rust libraries — provides patent-grant coverage Apache-2.0 has and MIT lacks).
- **Note on `Cargo.lock`:** since this workspace produces both libraries and binaries (`apps/*`), commit `Cargo.lock` at the workspace root (binaries should have a checked-in lockfile for reproducible builds; pure-library workspaces normally don't, but the mixed nature here means "commit it").
- **Acceptance criteria:** files exist; README renders correctly; `cargo build` (once workspace exists) does not warn about missing license fields.
- **Effort:** S
- **Depends on:** none

### Task 0.2 — Toolchain pin
- **Files:** `rust-toolchain.toml`
- **Description:** Pin `channel = "stable"` with `components = ["rustfmt", "clippy"]`. This controls what contributors/CI actually build with, independent of the MSRV floor declared in `Cargo.toml` (Task 0.3).
- **Acceptance criteria:** `rustup show` inside the repo reports the pinned toolchain.
- **Effort:** S
- **Depends on:** none

### Task 0.3 — Root workspace manifest
- **Files:** `Cargo.toml` (root)
- **Description:** Create `[workspace]` with `resolver = "2"` (explicit even though implied by edition 2021, for clarity in review) and `members = [...]` listing all 26 paths from the table above. Add `[workspace.package]`:
  ```toml
  [workspace.package]
  edition = "2021"
  rust-version = "1.78"
  license = "MIT OR Apache-2.0"
  repository = "<set to actual repo URL>"
  authors = ["<org/maintainer>"]
  ```
  Add `[workspace.dependencies]` centralizing every shared dependency version so member crates inherit via `dep.workspace = true`:
  ```toml
  [workspace.dependencies]
  tokio = { version = "1", default-features = false }
  tokio-util = "0.7"
  async-trait = "0.1"
  serde = { version = "1", features = ["derive"] }
  serde_json = "1"
  uuid = { version = "1", features = ["v4", "serde"] }
  rust_decimal = "1"
  rust_decimal_macros = "1"
  schemars = "0.8"
  thiserror = "1"
  anyhow = "1"
  tracing = "0.1"
  tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
  ```
  Verify exact current patch/minor versions via `cargo add <crate> --dry-run` (or crates.io) before finalizing — do not hand-guess patch numbers.
- **Rationale for `rust-version = "1.78"`:** conservative floor safely past native async-fn-in-trait stabilization (1.75) while still broadly available; adjust upward only if a chosen dependency requires it.
- **Acceptance criteria:** `cargo metadata` succeeds once member `Cargo.toml`s exist (Task 0.6/0.7).
- **Effort:** M
- **Depends on:** Task 0.2

### Task 0.4 — Format and lint configuration
- **Files:** `rustfmt.toml`, `clippy.toml`
- **Description:**
  `rustfmt.toml` (root, governs all member crates — rustfmt reads config from invocation root upward, do not duplicate per-crate):
  ```toml
  edition = "2021"
  imports_granularity = "Crate"
  group_imports = "StdExternalCrate"
  reorder_imports = true
  ```
  `clippy.toml` (root):
  ```toml
  msrv = "1.78"
  avoid-breaking-exported-api = true
  ```
  Add `#![warn(clippy::all)]` (optionally `#![warn(clippy::pedantic)]` as non-deny) to each crate's `lib.rs` root once crates are scaffolded (Task 0.6) — clippy lint *groups* are toggled via crate attributes or `-W`/`-D` CLI flags, not via `clippy.toml` (a common gotcha — `clippy.toml` only tunes lint *parameters*, e.g. `msrv`, thresholds).
- **Acceptance criteria:** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` run cleanly against the empty scaffolded crates.
- **Effort:** S
- **Depends on:** Task 0.3

### Task 0.5 — Supply-chain policy (`cargo-deny`)
- **Files:** `deny.toml`
- **Description:** Configure `[bans]`, `[licenses]` (allow `MIT`, `Apache-2.0`, `Unicode-DFS-2016`, etc. — the usual set compatible with `MIT OR Apache-2.0`), `[advisories]` (deny RustSec advisories), `[sources]` (deny unknown registries/git sources by default). This is the modern consolidated replacement for a standalone `cargo-audit` job.
- **Acceptance criteria:** `cargo deny check` passes with no dependencies yet (or trivially, once Task 0.3's deps are added).
- **Effort:** S
- **Depends on:** Task 0.3

### Task 0.6 — Scaffold core/runtime/engine/support library crates
- **Files:** for each of `harness-protocol`, `harness-core`, `harness-runtime`, `harness-engine`, `harness-model`, `harness-generic-backend`, `harness-tools`, `harness-context`, `harness-workspace`, `harness-session-store`, `harness-extension-api`:
  `crates/<name>/Cargo.toml`, `crates/<name>/src/lib.rs`
- **Description:** `cargo new --lib` equivalent for each, with `Cargo.toml` using workspace inheritance:
  ```toml
  [package]
  name = "harness-core"
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true
  version = "0.1.0"
  publish = false
  ```
  `src/lib.rs` starts as `#![warn(clippy::all)]` plus a doc comment stating the crate's single responsibility (one line each, taken from spec Section 5/65 — e.g. `harness-core`: "Deterministic Agent/Session domain semantics: state, transitions, commands, effects. No I/O."). No domain code yet — that begins in Phase 1.
  Do **not** add cross-crate dependencies yet beyond what's structurally obvious (e.g. `harness-core` depends on `harness-protocol`; `harness-runtime` depends on `harness-core` + `harness-protocol`); leave everything else for the phase that needs it, to avoid premature coupling.
- **Acceptance criteria:** `cargo check --workspace` succeeds; each crate's one-line purpose doc comment is present; no crate depends in the wrong direction (see Task 0.9).
- **Effort:** M
- **Depends on:** Task 0.3

### Task 0.7 — Scaffold integration / tool / transport crates
- **Files:** for each of the 6 `crates/integrations/*`, 3 `crates/tools/*`, 3 `crates/transports/*`: `Cargo.toml`, `src/lib.rs`
- **Description:** Same pattern as Task 0.6. Each crate's doc comment states which backend/tool/transport it will eventually implement and against which trait (e.g. `harness-integration-anthropic`: "Implements `ExecutionBackend` (via `GenericModelBackend` + a `ModelClient`) for the Anthropic Messages API. Empty until Phase 3."). These stay empty stubs until their respective phase (3, 4, 9/10).
- **Acceptance criteria:** `cargo check --workspace` still succeeds with all 12 crates added.
- **Effort:** M
- **Depends on:** Task 0.6

### Task 0.8 — Scaffold apps
- **Files:** `apps/harness/Cargo.toml` + `src/main.rs`, `apps/harnessd/Cargo.toml` + `src/main.rs`, `apps/harnessctl/Cargo.toml` + `src/main.rs`
- **Description:** `cargo new --bin` equivalent for each. `main.rs` contains only a `fn main() { println!("<name> — not yet implemented (see TASKS-09/10 phase docs)"); }` placeholder. These are not built out until Phases 8/9.
- **Acceptance criteria:** `cargo run -p harness` (and `-p harnessd`, `-p harnessctl`) prints the placeholder message.
- **Effort:** S
- **Depends on:** Task 0.6

### Task 0.9 — Dependency-direction guardrail
- **Files:** `xtask/Cargo.toml`, `xtask/src/main.rs` (kept as a standalone helper binary, not a workspace member, so it never affects `cargo build --workspace` timing)
- **Description:** Add a small `xtask` binary (standard Rust community pattern for repo automation) that runs `cargo metadata --format-version=1`, parses the dependency graph, and asserts the invariants from spec Section 66 / `TASKS-00-OVERVIEW.md` §3, e.g.:
  - `harness-core` and `harness-protocol` must not (transitively) depend on `tokio`'s networking features, `reqwest`, any `harness-integration-*`, any `harness-tool-*`, or `rusqlite`.
  - No crate under `crates/integrations/*` may be depended on by `harness-core`.
  Fail with a clear message naming the violating edge. This becomes a required CI job (Task 0.10) and turns an architectural principle into an automatically enforced one instead of a code-review-only rule.
- **Acceptance criteria:** `cargo run --manifest-path xtask/Cargo.toml -- check-deps` passes on the empty scaffold and fails with a clear diagnostic if a violating dependency is added.
- **Effort:** M
- **Depends on:** Task 0.6, 0.7

### Task 0.10 — CI pipeline
- **Files:** `.github/workflows/ci.yml`
- **Description:** Jobs (standard 2025–2026 Rust workspace pipeline):
  1. `fmt` — `cargo fmt --all -- --check`
  2. `clippy` — `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  3. `build` — `cargo build --workspace --all-targets`
  4. `test` — matrix over OS (`ubuntu-latest`, `macos-latest`, `windows-latest`) × toolchain (`stable`, pinned MSRV `1.78`; `beta` allow-failure)
  5. `doc` — `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
  6. `deny` — `cargo deny check`
  7. `deps` — `cargo run --manifest-path xtask/Cargo.toml -- check-deps` (Task 0.9)
  Use `dtolnay/rust-toolchain` (maintained, replaces the unmaintained `actions-rs`) and `Swatinem/rust-cache` for build caching.
- **Acceptance criteria:** CI is green on the empty scaffold; intentionally breaking one job locally (e.g. an unformatted file) causes it to fail when tested.
- **Effort:** M
- **Depends on:** Tasks 0.4, 0.5, 0.9

### Task 0.11 — Workspace smoke test
- **Files:** none (verification task)
- **Description:** Run `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `cargo deny check`, `cargo run --manifest-path xtask/Cargo.toml -- check-deps` locally and confirm all pass before declaring Phase 0 complete.
- **Acceptance criteria:** all six commands exit 0.
- **Effort:** S
- **Depends on:** all prior tasks in this phase

---

## Testing (this phase)

No domain tests yet. The "test" here is the CI pipeline itself plus the `xtask check-deps` guardrail — both are exercised in Task 0.11.

## Exit criteria

- `cargo build --workspace` succeeds with 26 members, all empty/stub.
- CI pipeline (fmt, clippy, build, test matrix, doc, deny, deps) is green.
- Dependency-direction check (`xtask check-deps`) is wired into CI and demonstrably fails on a deliberately-introduced bad edge.
- License, README, toolchain pin, and workspace dependency table are in place and will not need revisiting for the rest of the project.

## Open decisions flagged for later phases

- Exact current crate versions must be verified against crates.io at implementation time (research used knowledge current through ~2025; today is 2026-07-29).
- Whether `apps/*` binaries eventually need `tracing-subscriber`'s `json` feature (structured log shipping) is deferred to Phase 8/9.

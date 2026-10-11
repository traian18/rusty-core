# harness-session-store

Durable sessions. Every durable event is written as it happens, with periodic snapshots, so a session survives a restart and a reconnecting client can resume from a sequence number without gaps or duplicates. Raw streaming deltas stay ephemeral by design.

## What's inside

- `store` — the `SessionStore` contract and durable payloads.
- `jsonl` and `sqlite` — the two stores: append-only JSONL files, and a WAL-mode SQLite store behind a single-writer actor.
- `commit` — the authoritative event commit boundary.
- `replay`, `projection`, `resolver` — side-effect-free trailing validation and state reduction, and strict resolution of host dependencies on restore.
- `version`, `retention`, `diagnostics` — snapshot versioning and migration, lifecycle tooling.
- `testing` — an in-memory store with failure injection, for tests.

## In the workspace

- **Depends on:** `harness-protocol`
- **Used by:** `harness`, `harness-engine`, `harness-runtime`, `harnessd`, `rusty-harness-sdk`

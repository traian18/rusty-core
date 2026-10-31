# xtask

Repository maintenance tasks, kept outside the workspace (`exclude = ["xtask"]`) so it never ships in a build.

## What's inside

- `check-deps` — enforces the dependency direction. `rusty-agent-core` and `rusty-protocol` are protected: neither may depend on I/O, HTTP, database, UI, transport or provider crates (`reqwest`, `rusqlite`, `ratatui`, `tungstenite`, `rmcp`, …) or on `rusty-skills`, which walks the filesystem.

Run it from the repository root:

```sh
cargo run --manifest-path xtask/Cargo.toml -- check-deps
```

CI runs this check on every change.

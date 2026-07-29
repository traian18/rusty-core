# Rust Agent Harness

Rust Agent Harness is a reusable, embeddable Rust runtime for building tool-using AI agents. It separates deterministic agent and session semantics from asynchronous execution, model providers, tools, persistence, transports, and user interfaces so the same session behavior can run in a terminal, daemon, IDE, or host application.

This repository is organized as a Cargo workspace containing small crates with explicit responsibility boundaries. Phase 0 intentionally provides only buildable stubs; domain behavior is introduced by the ordered implementation phases.

## Project documentation

- [Development specification](rust-agent-harness-development-spec.md)
- [Implementation task overview](TASKS-00-OVERVIEW.md)
- [Phase 0 project setup](TASKS-01-PROJECT-SETUP.md)

## Development

The repository uses the stable Rust toolchain with Rust 1.78 as its minimum supported Rust version.

```console
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run --manifest-path xtask/Cargo.toml -- check-deps
```

Supply-chain checks use [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```console
cargo deny check
```

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.

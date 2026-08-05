# `rusty-harness-sdk` (Rust)

Stable, application-agnostic Rust facade over the Rusty agent harness engine.

## Status

Alpha. This crate compiles against the current workspace `harness-engine`
API and is safe to depend on for prototyping and internal integration.
It is **not yet** a semver-guaranteed public release: see
[`../../sdk_plan.md`](../../sdk_plan.md) for the governance and release
gates this crate must pass before a 1.0.

Known, tracked runtime limitations that this SDK inherits from the engine
(not fixed by this facade layer) are documented inline on `Client` and
`Session` and summarized in [`../../upgrade_rusty.md`](../../upgrade_rusty.md):

- Busy-session prompt admission (steer/follow-up/queue) is not yet a stable
  contract (`upgrade_rusty.md` RST-002/RST-003).
- Session restore rebuilds from the last snapshot only; trailing durable
  events are not yet replayed, and restored sessions do not yet reattach a
  real workspace/tool/credential environment (RST-008/RST-009).

## Why this crate exists

Applications embedding the harness today depend directly on
`harness-engine` plus whichever internal crates they need for types
(`harness-protocol`, `harness-runtime`, `harness-session-store`). Those
crates are workspace-internal and can change shape between versions.

`rusty-harness-sdk` re-exports a curated, documented subset of that surface
under one crate name and adds small ergonomic wrappers:

- [`Client`] / [`ClientBuilder`] — thin wrapper over `Harness`/`HarnessBuilder`.
- [`Session`] — wraps `SessionHandle`, converts engine errors into
  [`SdkError`], and exposes an `EventStream` instead of a raw
  `tokio::sync::broadcast::Receiver`.
- [`EventStream`] — implements `futures::Stream` and a plain `async fn next`,
  and turns broadcast lag into a typed, catchable error instead of a panic
  or silent gap.
- `protocol`, `providers`, `integration`, and `store` modules — curated
  re-exports so consumers rarely need to add `harness-protocol` /
  `harness-runtime` / `harness-session-store` as direct dependencies.

Applications that are not Rust, or that want the engine in a separate
process/sandbox, should run `harnessd` and speak the wire protocol instead
— see [`../typescript`](../typescript) and
[`../../schema/protocol-v1.schema.json`](../../schema/protocol-v1.schema.json).
Both integration modes are meant to expose equivalent session/event/
permission semantics (see `sdk_plan.md` §2, "One behavioral contract").

## Quick start

```rust,no_run
use rusty_harness_sdk::{Client, Session};

# async fn run() -> Result<(), rusty_harness_sdk::SdkError> {
let client = Client::builder().build().await?;

let handle = client
    .session()
    .integration("anthropic", serde_json::json!({}))?
    .start()
    .await?;
let session = Session::from(handle);

let mut events = session.events();
session.send("hello").await?;
while let Some(event) = events.next().await {
    let _event = event?;
}
# Ok(())
# }
```

See `examples/basic_chat.rs` for a runnable version that registers a real
integration and prints streamed text.

## What this SDK does not yet do

- No `steer`, `follow_up`, or `close_session` operations (engine gap, not
  just an SDK gap — see `sdk_plan.md` SDK-101).
- No idempotent command IDs / admission responses (SDK-102).
- No model/credential registry wrapper beyond re-exporting the engine's
  existing provider discovery methods (`Client::engine().list_models(..)`).
- No attachment support on `Session::send` (the engine's `send` always
  sends an empty attachment list today).

These are tracked in [`../../sdk_plan.md`](../../sdk_plan.md) Phase 1–5 and
are not silently worked around here — the goal of this crate is to make the
current, real engine behavior easy to consume, not to paper over gaps with
an API that promises more than the engine delivers.

# rusty-harness-sdk

A stable, application-agnostic Rust facade over the harness engine. Alpha: it compiles against the
current workspace `rusty-engine` and is safe for prototyping and internal integration, but is not yet a
semver-guaranteed release.

## Why it exists

Applications embedding the harness would otherwise depend on `rusty-engine` plus the internal crates
for types (`rusty-protocol`, `rusty-runtime`, `rusty-session-store`), which can change shape between
versions. The SDK re-exports a curated, documented subset under one name and adds small wrappers.

## How it works

1. `Client` / `ClientBuilder` wrap `Harness` / `HarnessBuilder`.
2. `Session` wraps `SessionHandle`, converts engine errors to `SdkError`, and exposes an `EventStream`
   instead of a raw broadcast receiver.
3. `EventStream` implements `futures::Stream` and a plain `next`, and turns broadcast lag into a typed
   error instead of a silent gap.
4. The `protocol`, `providers`, `integration` and `store` modules re-export what consumers need, so
   they rarely add the internal crates themselves.

## Key types and concepts

- **`Client`**: builds sessions and reaches the engine for provider discovery.
- **`Session`**: `send`, `steer`, `follow_up`, `cancel`, `close`, `resolve_permission`, and
  `events()` to subscribe. `Client` also lists and restores stored sessions.
- **`SdkError`**: the one error type.
- **Wire alternative**: non-Rust applications, or ones that want the engine in another process, run
  `harnessd` and speak the protocol (`sdk/typescript`, `schema/protocol-v2.schema.json`).

## A worked example

See the quick start below; `examples/basic_chat.rs` is a runnable version.

## Public API

- `Client`, `ClientBuilder`, `Session`, `EventStream`, `SdkError`, `McpServerConfig`,
  `McpTransportConfig`, and the `protocol`, `providers`, `integration` and `store` modules.

## Errors

`SdkError` wraps engine errors and event-stream problems (including lag) in one catchable type.

## Design decisions

- **Real behavior, not promises.** Gaps in the engine (steer, follow-up, close, command ids) are not
  papered over; they are listed under "What this SDK does not yet do".
- **`client.session()` is the engine's `SessionBuilder`**, so every builder option works unchanged.

## Testing

Facade tests cover event streaming and lag. Run `cargo test -p rusty-harness-sdk`.

## In the workspace

- **Depends on:** `rusty-engine`, `rusty-protocol`, `rusty-runtime`, `rusty-session-store`, `rusty-workspace`
- **Used by:** nothing else in the workspace

## Must never

- Promise behavior the engine does not deliver.
- Drop events silently.
- Add logic of its own beyond wrapping.

## Usage guide

### Quick start

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

### MCP servers

`client.session()` returns `rusty_engine::SessionBuilder` directly (see
"Why this crate exists" above), so [MCP](https://modelcontextprotocol.io)
server support needs no wrapper here — `SessionBuilder::mcp_server` already
works exactly as documented in the main
[README's MCP servers section](../../README.md#mcp-servers):

```rust,no_run
use rusty_harness_sdk::{Client, McpServerConfig, Session};

# async fn run() -> Result<(), rusty_harness_sdk::SdkError> {
let client = Client::builder().build().await?;

let handle = client
    .session()
    .integration("anthropic", serde_json::json!({}))?
    .mcp_server(
        McpServerConfig::new("filesystem", "npx")
            .args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]),
    )
    .start()
    .await?;
let session = Session::from(handle);
# Ok(())
# }
```

### What this SDK does not yet do

- No idempotent command IDs or admission responses: `send`, `steer`, `follow_up`, `cancel` and
  `close` return only success or an error.
- No model/credential registry wrapper beyond re-exporting the engine's
  existing provider discovery methods (`Client::engine().list_models(..)`).
- No attachment support on `Session::send` (the engine's `send` always
  sends an empty attachment list today).

These gaps are not silently worked around here — the goal of this crate is to make the
current, real engine behavior easy to consume, not to paper over gaps with
an API that promises more than the engine delivers.

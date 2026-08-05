# `@rusty/harness-sdk` (TypeScript)

Application-agnostic TypeScript client for the `harnessd` wire protocol.

## Status

Alpha, source-only. This package is a hand-written client for protocol v1
(`schema/protocol-v1.schema.json`) as it exists today. It is not yet
published to npm, has not been run through `tsc`/`node --test` in this
change (see "Verification" below), and its public API is not
semver-guaranteed. See [`../../sdk_plan.md`](../../sdk_plan.md) SDK-301 for
the full production requirements this package must still meet (runtime
frame validation, WebSocket transport, reconnect/cursor persistence,
delta-assembly helpers, mock transport for tests, CJS policy).

## What's here

- `src/types.ts` — hand-authored mirror of `harness-protocol`'s Rust types,
  including serde's externally-tagged enum representation.
- `src/errors.ts` — typed errors (`HarnessRpcError`, `HarnessTimeoutError`,
  `HarnessVersionMismatchError`, `HarnessTransportClosedError`,
  `HarnessProtocolError`).
- `src/transport.ts` — `Transport` interface plus `StdioSidecarTransport`,
  a managed-sidecar transport that spawns `harnessd --stdio` and frames
  newline-delimited JSON, matching `crates/transports/stdio`.
- `src/client.ts` — `HarnessClient`: connects (mandatory `Hello` handshake),
  creates sessions, and dispatches `Prompt`/`Cancel`/`ResolvePermission`/
  `Snapshot`/`Subscribe`/`CloseSession` by correlation ID.
- `src/session.ts` — `HarnessSession`: a per-session ergonomic wrapper,
  including an `events()` async generator in addition to the callback-based
  `subscribe()`.

## What's intentionally not here yet

- **WebSocket / IPC transports.** Only the stdio sidecar is implemented.
  `Transport` is designed so a WebSocket transport is a drop-in addition.
- **Runtime frame validation.** Decoded JSON is cast, not validated, against
  `types.ts`. A schema-validation layer (from `schema/protocol-v1.schema.json`)
  is planned in SDK-201/SDK-301 rather than hand-maintained twice.
- **Reconnect/resume helpers.** `subscribe(sinceSeq)` exists, but there is no
  cursor-persistence helper or automatic resubscribe-on-reconnect yet.
- **Structured errors from the daemon.** `RpcResponseBody::Error` is a bare
  string on the wire today (`sdk_plan.md` SDK-200); `HarnessRpcError.message`
  is that raw string, not a typed code.
- **A mock transport for tests.** Tests should implement `Transport`
  directly against fixtures in `sdk/conformance` once that suite exists.

## Verification

This package has not been built or type-checked as part of this change
(`npm`/`tsc` were not run against it). Before depending on it:

```console
cd sdk/typescript
npm install
npm run build
```

## Example

See `examples/basic-chat.ts` — spawns `harnessd --stdio`, creates an
`anthropic` session, sends one prompt, and prints streamed text.

# harnessctl

A reference command-line client for a running `harnessd` over the IPC transport. It is an operational
tool for debugging and scripting, and a worked example for anyone writing a real client against the same
wire protocol.

## Why it exists

A protocol needs a client that proves it is usable. `harnessctl` is small enough to read in one
sitting and complete enough to run a real session, including approving tool permissions.

## How it works

1. `client::HarnessClient` connects to the Unix socket, performs the `Hello` handshake, sends
   requests with correlation ids and matches responses.
2. Subcommands wrap one request each: `session create | send | events | snapshot | cancel | pause |
   resume | close` and `permission resolve`.
3. `chat` creates a session and opens an interactive terminal UI: type a prompt, watch it stream, and
   approve or reject permission requests inline.

## Key types and concepts

- **Correlation id**: ties a response to its request.
- **Event stream**: after `subscribe`, events arrive as they happen, with sequence numbers for replay.

## A worked example

```console
harnessctl --socket /tmp/harness.sock session create
harnessctl --socket /tmp/harness.sock session send <id> "summarise this repo"
harnessctl --socket /tmp/harness.sock session events <id>
```

## Public API

A binary with the subcommands above; `HarnessClient` is the part worth copying.

## Errors

A failed handshake, a version mismatch or a `Failure` response is printed with its code and exits
non-zero.

## Design decisions

- **Depends only on the protocol and the IPC transport**, as any external client would.
- **No engine types**, to keep it a faithful example.

## Testing

Exercised through the daemon's end-to-end suite. Run `cargo test -p harnessctl`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-transport-ipc`
- **Used by:** nothing else in the workspace

## Must never

- Depend on the engine or the runtime.
- Assume an event arrived because a request succeeded; it reads the stream.

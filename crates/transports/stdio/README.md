# harness-transport-stdio

The same `RpcHandler` contract over stdin/stdout using newline-delimited JSON, one message per line. Suited to an IDE that spawns the daemon as a child process.

## What's inside

- Reads requests from stdin and writes responses and events to stdout.

**Stdout is owned by the transport.** Nothing else may write to it, which is why `harnessd` routes all logging to stderr.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-runtime`
- **Used by:** `harnessd`

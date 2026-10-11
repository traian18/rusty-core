# harness-tool-shell

Cancellable command execution for agents.

## What's inside

- `shell.exec` — run a command (with arguments, working directory and an optional timeout); cancelling the call stops the process.

Commands are normally gated by the permission system (Ask) rather than trusted by default.

## In the workspace

- **Depends on:** `harness-tools`, `harness-workspace`
- **Used by:** `harness-engine`

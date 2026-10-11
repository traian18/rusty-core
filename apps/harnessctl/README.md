# harnessctl

A reference command-line client for a running `harnessd` over the IPC transport. It is an operational tool for debugging and scripting, and a worked example for anyone writing a real client against the same wire protocol.

## What's inside

- `chat` — create a session and drop into an interactive terminal UI: type a prompt, watch it stream, approve or reject permission prompts inline.
- `session create | send | events | snapshot | cancel | pause | resume | close` and `permission resolve` — the scriptable equivalents.
- `client` — a small `HarnessClient` showing the handshake, request/response and event handling.

## In the workspace

- **Depends on:** `harness-protocol`, `harness-transport-ipc`
- **Used by:** applications and external embedders (a leaf of the workspace)

# harness-runtime

The async half of the engine. It takes the effects `harness-core` produces and carries them out: it runs sessions and agents on Tokio, calls the model backend, executes tools, enforces permissions and cancellation, persists events, and feeds every outcome back to the core as a command.

## What's inside

- `traits` — the behavioral traits other crates implement: `ExecutionBackend`, `Workspace`, tool registry glue.
- `session_manager`, `session_runtime`, `restore` — concurrent sessions, the authoritative commit boundary to the session store, snapshots/checkpoints and strict restore.
- `agent_runner`, `agent_supervisor`, `spawn_tool` — the agent loop, child agents (`agent_spawn`) with depth and budget limits.
- `scheduler`, `resource_manager`, `cancellation` — concurrency caps for sessions and agents, shared limits, hierarchical cancellation.
- `permissions`, `completion_gate`, `scoped_tools` — Allow/Ask/Deny gating, completion-gate evaluators, per-agent tool scoping.
- `orchestration`, `task_queue`, `session_agent_executor` — runs workflow definitions (each agent step in an isolated session), subflows, and the persisted serial task queue.
- `rpc`, `session_client` — the `RpcHandler` contract that transports serve, and an in-process client.
- `testing` — fakes (`FakeBackend`, fake workspaces) behind the `testing` feature.

## In the workspace

- **Depends on:** `harness-core`, `harness-protocol`, `harness-session-store`, `harness-tools`, `harness-workspace`
- **Used by:** `harness-context`, `harness-engine`, `harness-extension-api`, `harness-generic-backend`, `harness-integration-anthropic`, `harness-integration-codex`, `harness-integration-gemini`, `harness-integration-github-copilot`, `harness-integration-openai`, `harness-integration-openai-compatible`, `harness-integration-openai-responses`, `harness-transport-ipc`, `harness-transport-mcp`, `harness-transport-stdio`, `harness-transport-websocket`, `harnessd`, `rusty-harness-sdk`

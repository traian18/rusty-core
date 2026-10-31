# rusty-runtime

The async half of the engine. It takes the effects `rusty-agent-core` produces and carries them out: it runs sessions and agents on Tokio, calls the model backend, executes tools, enforces permissions and cancellation, persists events, and feeds every outcome back to the core as a command.

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

- **Depends on:** `rusty-agent-core`, `rusty-protocol`, `rusty-session-store`, `rusty-tool-api`, `rusty-workspace`
- **Used by:** `rusty-context`, `rusty-engine`, `rusty-extension-api`, `rusty-model-backend`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-gemini`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-provider-openai-compatible`, `rusty-provider-openai-responses`, `rusty-transport-ipc`, `rusty-transport-mcp`, `rusty-transport-stdio`, `rusty-transport-websocket`, `harnessd`, `rusty-harness-sdk`

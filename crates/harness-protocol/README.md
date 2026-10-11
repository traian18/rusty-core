# harness-protocol

The shared vocabulary of the harness: every ID, command, event, message and wire type that crosses a crate or process boundary. It has no runtime and does no I/O, so anything (the deterministic core, the async runtime, a transport, an SDK, an IDE) can depend on it without pulling in policy.

## What's inside

- `ids` — strongly typed UUID identifiers and timestamps (`SessionId`, `AgentId`, `RunId`, `MessageId`, `ToolCallId`, …).
- `commands` / `events` / `effects` — what can be asked of an agent, what it emits (`AgentEventEnvelope` with routing metadata and two monotonic sequence numbers), and what the core asks the runtime to do.
- `messages` — provider-neutral conversation content (`AgentMessage`, `ContentBlock`: text, tool use, tool result, image) and tool-history repair.
- `backend` — the `ExecutionRequest` / `ExecutionEvent` / `ExecutionResult` contract between the agent loop and a model backend, including `ExecutionParams`.
- `tools`, `usage`, `mcp`, `skills`, `admission`, `lifecycle` — tool descriptors and execution policy, token/cost accounting, MCP and skill specs, run admission and lifecycle types.
- `rpc` — the transport-neutral JSON RPC contract (protocol v2) shared by every transport and SDK.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `harness`, `harness-context`, `harness-core`, `harness-engine`, `harness-generic-backend`, `harness-integration-anthropic`, `harness-integration-codex`, `harness-integration-gemini`, `harness-integration-github-copilot`, `harness-integration-openai`, `harness-integration-openai-compatible`, `harness-integration-openai-responses`, `harness-model`, `harness-runtime`, `harness-session-store`, `harness-skills`, `harness-tool-mcp`, `harness-transport-ipc`, `harness-transport-mcp`, `harness-transport-stdio`, `harness-transport-websocket`, `harnessctl`, `harnessd`, `rusty-harness-sdk`

# harness-model

Provider-neutral model types: requests, streaming responses, tool calls, usage, and the `ModelClient` trait that every provider implements. It exists so the many provider adapters can share one backend implementation instead of each re-implementing the agent contract.

## What's inside

- `client` — the dyn-compatible `ModelClient` trait.
- `request`, `events`, `provider_options` — the neutral request/stream shapes and namespaced provider-specific options.
- `retry` — normalization of provider retry hints (`Retry-After` and provider-specific millisecond headers).
- `auth` — HTTP inference authentication; implementors never receive an execution handle.

## In the workspace

- **Depends on:** `harness-protocol`
- **Used by:** `harness-extension-api`, `harness-generic-backend`, `harness-integration-anthropic`, `harness-integration-codex`, `harness-integration-gemini`, `harness-integration-github-copilot`, `harness-integration-openai`, `harness-integration-openai-responses`

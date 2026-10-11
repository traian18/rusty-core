# harness-generic-backend

`GenericModelBackend`: one `ExecutionBackend` implementation that works with any `ModelClient`. Every integration crate only has to translate the provider's wire format; the shared behavior (streaming, tool-call assembly, retries with backoff and jitter under a shared deadline, circuit breaking, usage and cost reporting) lives here once.

## What's inside

- `backend` — `GenericModelBackend` and its configuration.
- `testing` — fake model clients for backend tests.

## In the workspace

- **Depends on:** `harness-model`, `harness-protocol`, `harness-runtime`
- **Used by:** `harness-extension-api`, `harness-integration-anthropic`, `harness-integration-codex`, `harness-integration-gemini`, `harness-integration-github-copilot`, `harness-integration-openai`, `harness-integration-openai-compatible`, `harness-integration-openai-responses`

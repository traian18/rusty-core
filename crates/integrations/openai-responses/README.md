# harness-integration-openai-responses

Model backend for the OpenAI Responses API (`{base_url}/responses`), which is a different wire shape from Chat Completions. It is also the transport the Codex subscription adapter builds on.

## What's inside

- Integration id: `openai-responses`.
- `wire`, `client`, `config`, `usage` — Responses input/output items, the `ModelClient`, configuration and usage.

## In the workspace

- **Depends on:** `harness-generic-backend`, `harness-model`, `harness-protocol`, `harness-runtime`
- **Used by:** `harness-integration-codex`, `harness-integration-github-copilot`

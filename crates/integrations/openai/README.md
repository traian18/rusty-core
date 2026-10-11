# harness-integration-openai

Model backend for the OpenAI Chat Completions API. Also the foundation for every OpenAI-shaped endpoint: `harness-integration-openai-compatible` reuses its client.

## What's inside

- Integration id: `openai`.
- `wire`, `client`, `config`, `usage` — wire types, the `ModelClient`, configuration and usage accounting, including reasoning-effort mapping.

## In the workspace

- **Depends on:** `harness-generic-backend`, `harness-model`, `harness-protocol`, `harness-runtime`
- **Used by:** `harness`, `harness-integration-github-copilot`, `harness-integration-openai-compatible`, `harnessd`

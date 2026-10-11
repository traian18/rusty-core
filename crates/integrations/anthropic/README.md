# harness-integration-anthropic

Model backend for the Anthropic Messages API. It translates between the harness's neutral messages and Anthropic's wire format and plugs into `GenericModelBackend`; retries, streaming and tool-call handling are shared.

## What's inside

- Integration id: `anthropic` (API key from config or `ANTHROPIC_API_KEY`).
- `wire`, `client`, `config`, `usage` — wire types, the `ModelClient`, configuration and token/cost accounting.

Inference only: tools always execute through the harness, never inside the provider.

## In the workspace

- **Depends on:** `harness-generic-backend`, `harness-model`, `harness-protocol`, `harness-runtime`
- **Used by:** `harness`, `harness-integration-github-copilot`, `harnessd`

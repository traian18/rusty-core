# harness-integration-openai-compatible

Model backend for any endpoint that speaks OpenAI Chat Completions: OpenRouter, Together, Groq, or a local Ollama, vLLM or llama.cpp server. It reuses `harness-integration-openai`'s client directly and is parameterized by base URL, API key, model and extra headers, so it has no wire-format or SSE parsing of its own.

## What's inside

- Integration id: `openai-compatible`.
- `config` — base URL, key, model and headers.

## In the workspace

- **Depends on:** `harness-generic-backend`, `harness-integration-openai`, `harness-protocol`, `harness-runtime`
- **Used by:** `harnessd`

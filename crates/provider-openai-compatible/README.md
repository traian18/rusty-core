# rusty-provider-openai-compatible

Model backend for any endpoint that speaks OpenAI Chat Completions: OpenRouter, Together,
Groq, or a local Ollama, vLLM or llama.cpp server. It reuses `rusty-provider-openai`'s client
directly and is parameterized by base URL, API key, model and extra headers, so it has no wire
format or SSE parsing of its own. Integration id: `openai-compatible`.

## Why it exists

Dozens of services copy the Chat Completions shape. Rather than a crate per service, one crate
adapts configuration (a base URL and a key) onto the OpenAI client. A new compatible service
needs a config entry, not code.

## How it works

`OpenAiCompatibleFactory` reads the integration config (`config`), builds the OpenAI client's
configuration from it, constructs an `OpenAiClient`, and wraps it in `GenericModelBackend`. All
request building, streaming and error handling is `rusty-provider-openai`'s.

## Key types and concepts

- **`OpenAiCompatibleConfig`**: `base_url`, an optional `api_key` (local servers need none),
  `model`, `default_max_tokens`, `request_timeout`, a `recovery` policy, `extra_headers`, and
  `supports_reasoning`.
- **`OpenAiCompatibleBackend`, `OpenAiCompatibleFactory`**: the wrapped backend and the
  integration factory registered by the daemon.

## A worked example

A local server needs only a URL:

```json
{ "base_url": "http://localhost:11434/v1", "model": "qwen2.5-coder" }
```

An OpenRouter model adds a key: `{ "base_url": "https://openrouter.ai/api/v1", "api_key": "...",
"model": "anthropic/claude-sonnet-4" }`. Usage that OpenRouter reports out of band is recovered by
the OpenAI client.

## Public API

- `OpenAiCompatibleBackend`, `OpenAiCompatibleFactory`, `OpenAiCompatibleConfig`.

## Errors

Configuration problems (missing base URL, unparseable headers) surface as
`IntegrationError` when the factory builds the backend. Runtime errors are `rusty-provider-openai`'s.

## Design decisions

- **No wire code here.** Anything that is not configuration belongs in `rusty-provider-openai`,
  so every compatible endpoint benefits from a fix.
- **Reasoning is opt-in per endpoint.** Many compatible servers reject `reasoning_effort`.

## Testing

`src/config.rs` has unit tests for configuration parsing and defaults. Wire behavior is tested in
`rusty-provider-openai`. Run `cargo test -p rusty-provider-openai-compatible`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-provider-openai`, `rusty-runtime`
- **Used by:** `harnessd`

## Must never

- Parse SSE or build request bodies; that is `rusty-provider-openai`'s job.
- Send credentials anywhere but the configured base URL.

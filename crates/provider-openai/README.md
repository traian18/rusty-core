# rusty-provider-openai

Model backend for the OpenAI Chat Completions API (`{base_url}/chat/completions`), and the
foundation for every OpenAI-shaped endpoint: `rusty-provider-openai-compatible` reuses its client.
Integration id: `openai`.

## Why it exists

A provider's wire format is the only thing that differs between model APIs; streaming, retries,
timeouts, circuit breaking, cost reporting and tool-call assembly are the same everywhere. So
each provider is a thin crate that implements `ModelClient` for one API, and
`rusty-model-backend` wraps it in `GenericModelBackend` to get everything else once. Adding a
provider means writing its request and response mapping and nothing more.

Chat Completions became the de facto wire format for many gateways, so this crate also carries
the small dialect differences among them (reasoning text under `reasoning_content` or
`reasoning`, usage on a separate or on the final chunk, OpenRouter's out-of-band usage lookup).

## How it works

`OpenAiClient::stream` is three steps: `build_body`, `send_request`, `read_response`.

- **Request** (`client/request.rs`, `wire/`): messages and tools are converted, `stream_options`
  asks for usage, a `response_format` is set when structured output is requested, and a
  `reasoning_effort` is sent only when the endpoint is configured to support reasoning. Caller
  provider options and prompt-cache hints (`cache`) are merged into the body.
- **Response** (`wire/sse/`): `data:` frames become events. `chunk.rs` records metadata (response
  id, model, usage), then turns the first choice's delta into a reasoning, text or tool-call
  event. Tool calls are only *completed* when the stream ends, because the API has no per-call
  stop signal.
- **OpenRouter** (`client/response.rs`): when usage is missing from the stream, the client looks
  it up from OpenRouter's generation record, retrying until it is populated.

## Key types and concepts

- **`OpenAiClient`, `OpenAiConfig`, `OpenAiBackend`, `OpenAiFactory`**: client, settings,
  backend and factory.
- **`OpenAiSseParser`**: the incremental parser; `finish` flushes tool calls and the final usage.
- **`ProviderToolIds`**: harness tool-call ids mapped to the provider's.
- **`OpenAiResponseFormat`**: structured output requests (JSON schema).
- **Usage mapping and prompt-cache hints** (`usage`, `cache`).

## A worked example

Two chunks of a streamed tool call:

```text
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_9","function":{"name":"fs.read","arguments":"{\"pa"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}
data: [DONE]
```

The first chunk yields `ToolCallStarted`, the second a `ToolCallDelta`, and `finish()` yields
`ToolCallCompleted` with the assembled JSON followed by `Completed`.

## Public API

- `OpenAiBackend`, `OpenAiFactory`, `OpenAiConfig`.
- `client::OpenAiClient`, `wire` (request types, conversion, `OpenAiSseParser`), `usage`, `cache`,
  `config`.

## Errors

`ModelError::BackendError` carries the API's error `type` as its code; `RateLimited` carries the
parsed `Retry-After`; a stream that ends without `[DONE]` is `StreamInterrupted`; unparseable
frames or tool arguments are `Protocol`.

## Design decisions

- **Tool calls complete at the end.** The API gives no per-call terminator, so they are flushed in
  `finish` in index order.
- **Reasoning is capability-gated.** `reasoning_effort` is only sent when configured, since many
  compatible endpoints reject unknown parameters.
- **One parser for many gateways.** Dialect handling lives in the parser, not in per-gateway
  crates.

## Testing

`src/wire/tests.rs` covers conversion and the parser on recorded streams; `src/client/tests.rs`
covers `Retry-After` and the OpenRouter usage lookup against a local test server. Run
`cargo test -p rusty-provider-openai`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-runtime`
- **Used by:** `harness`, `harnessd`, `rusty-provider-github-copilot`, `rusty-provider-openai-compatible`

## Must never

- Execute tools: a model response only *proposes* tool calls; the harness authorizes and runs them.
- Implement retries, circuit breaking or timeouts of its own; `GenericModelBackend` owns those.
- Depend on the engine, the runtime's session types, or another provider's internals (except the
  OpenAI-shaped providers, which share `rusty-provider-openai`).

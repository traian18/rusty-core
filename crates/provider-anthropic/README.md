# rusty-provider-anthropic

Model backend for the Anthropic Messages API. It translates between the harness's neutral
messages and Anthropic's wire format and plugs into `GenericModelBackend`. Integration id:
`anthropic` (API key from config or `ANTHROPIC_API_KEY`). Inference only: tools always execute
through the harness, never inside the provider.

## Why it exists

A provider's wire format is the only thing that differs between model APIs; streaming, retries,
timeouts, circuit breaking, cost reporting and tool-call assembly are the same everywhere. So
each provider is a thin crate that implements `ModelClient` for one API, and
`rusty-model-backend` wraps it in `GenericModelBackend` to get everything else once. Adding a
provider means writing its request and response mapping and nothing more.

Anthropic has several quirks worth isolating in one place: tool-use ids that must be echoed back
exactly, thinking modes that differ by model generation, a prompt-cache API, and gateways that
answer with one JSON message instead of an event stream.

## How it works

`AnthropicClient::stream` is three steps: `build_body`, `send_request`, `read_response`.

- **Request** (`client/request.rs`, `wire/`): the conversation becomes Messages API messages
  (`wire/convert.rs`), tools become tool definitions, and the thinking mode is chosen per model
  (`wire/thinking.rs`): newer models require adaptive thinking, older ones take a token budget. A
  request for structured output uses a synthetic tool whose input is re-emitted as text.
- **Response** (`wire/sse/`): the SSE stream is parsed incrementally into `ModelEvent`s. Each event
  kind (`message_start`, `content_block_start`, `content_block_delta`, `content_block_stop`,
  `message_delta`, `error`) has its own small handler in `wire/sse/block.rs`. If a gateway
  returns one complete JSON message instead, `wire/sse/non_streaming.rs` handles that.
- **Errors** (`client/response.rs`): `Retry-After` headers become retry delays, and a
  "max_tokens too large" error is parsed for the model's real ceiling so the client can retry
  below it.

## Key types and concepts

- **`AnthropicClient`, `AnthropicConfig`, `AnthropicBackend`, `AnthropicFactory`**: the client,
  its settings, the wrapped backend and the integration factory.
- **`AnthropicSseParser`**: the incremental stream parser; accepts arbitrary HTTP chunks.
- **`ProviderToolIds`**: a map from the harness's tool call ids to Anthropic's, because the next
  request must name the provider's id.
- **Usage mapping** (`usage`): input, output, cache-read and cache-write tokens, and cost.
- **Prompt cache** (`cache`): cache-control hints placed on the system prompt and tools.

## A worked example

A tool call round trip, as the parser sees it:

```text
event: content_block_start   {"index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"fs.read"}}
event: content_block_delta   {"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}
event: content_block_delta   {"index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}
event: content_block_stop    {"index":1}
```

becomes `ToolCallStarted { name: "fs.read" }`, two `ToolCallDelta`s and
`ToolCallCompleted { input: {"path":"a.rs"} }`. The harness's id for the call is mapped to
`toolu_01` so the tool result in the next request names the id Anthropic issued.

## Public API

- `AnthropicBackend`, `AnthropicFactory`, `AnthropicConfig`.
- `client::AnthropicClient`, `wire` (request types, conversion, `AnthropicSseParser`), `usage`,
  `cache`, `config`.

## Errors

Failures are `ModelError`s: `BackendError` with Anthropic's error `type` as the code,
`RateLimited` with the parsed delay, `Protocol` for malformed events, `StreamInterrupted` when
the stream ends without `message_stop`, and `InvalidRequest` for tool names Anthropic does not
allow (it restricts the characters).

## Design decisions

- **Thinking is chosen per model, and learned.** A model that rejects a fixed budget is
  remembered as adaptive-only for the rest of the process.
- **Message conversion is fallible, not panicking.** A system message in the list converts to
  nothing instead of aborting; system text travels in the request's `system` field.
- **Tool ids are mapped, never guessed.** The map tolerates a poisoned lock.
- **The structured-output tool is invisible.** It never appears as a tool call to the host.

## Testing

`src/wire/tests.rs` covers conversion, thinking selection and the SSE parser against recorded
streams; `src/client/tests.rs` covers `Retry-After` handling and the max-tokens ceiling parser;
`tests/session_e2e.rs` runs a session through the backend. Run
`cargo test -p rusty-provider-anthropic`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-runtime`
- **Used by:** `harness`, `harnessd`, `rusty-provider-github-copilot`

## Must never

- Execute tools: a model response only *proposes* tool calls; the harness authorizes and runs them.
- Implement retries, circuit breaking or timeouts of its own; `GenericModelBackend` owns those.
- Depend on the engine, the runtime's session types, or another provider's internals (except the
  OpenAI-shaped providers, which share `rusty-provider-openai`).

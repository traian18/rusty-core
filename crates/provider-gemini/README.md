# rusty-provider-gemini

Model backend for the Google Gemini API (streaming `generateContent`). Integration id:
`gemini`.

## Why it exists

A provider's wire format is the only thing that differs between model APIs; streaming, retries,
timeouts, circuit breaking, cost reporting and tool-call assembly are the same everywhere. So
each provider is a thin crate that implements `ModelClient` for one API, and
`rusty-model-backend` wraps it in `GenericModelBackend` to get everything else once. Adding a
provider means writing its request and response mapping and nothing more.

Gemini's shapes differ from both Anthropic's and OpenAI's: conversation turns are `contents`
made of `parts`, function calls arrive whole in one part, and no call id exists.

## How it works

`GeminiClient::stream` converts the conversation into `contents` and tool declarations, posts to
the streaming endpoint with the API key, and feeds the response to `GeminiSseParser` (`wire.rs`).
Each `data:` frame may carry `usageMetadata`, and a candidate whose `content.parts` hold text
and/or `functionCall`s. `part_events` turns a part into events; `finishReason` marks the stream
as properly finished.

## Key types and concepts

- **`GeminiClient`, `GeminiConfig`, `GeminiBackend`, `GeminiFactory`**.
- **`GeminiSseParser`**: the incremental parser; requires a `finishReason` before it will finish.
- **Usage mapping** (`usage`): prompt, candidate and cached token counts and cost.

## A worked example

```text
data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"fs.read","args":{"path":"a.rs"}}}]}}]}
data: {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":120,"candidatesTokenCount":9}}
```

The first frame yields `ToolCallStarted` and `ToolCallCompleted` together (Gemini sends the call
whole); the second yields a usage update, and the stream finishes.

## Public API

- `GeminiBackend`, `GeminiFactory`, `GeminiConfig`.
- `client::GeminiClient`, `wire` (content types, `GeminiSseParser`), `usage`.

## Errors

An `error` object in a frame becomes `BackendError` with Gemini's `status` as the code; a stream
that ends without a `finishReason` is `StreamInterrupted`; malformed frames are `Protocol`.

## Design decisions

- **Function calls need no id mapping.** Gemini provides none, so a fresh internal id is minted
  for each call and never mapped back.
- **A finish reason is required.** Without one the stream is treated as cut short.

## Testing

`src/wire/tests.rs` covers conversion and the parser on recorded streams. Run
`cargo test -p rusty-provider-gemini`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-runtime`
- **Used by:** `harnessd`

## Must never

- Execute tools: a model response only *proposes* tool calls; the harness authorizes and runs them.
- Implement retries, circuit breaking or timeouts of its own; `GenericModelBackend` owns those.
- Depend on the engine, the runtime's session types, or another provider's internals (except the
  OpenAI-shaped providers, which share `rusty-provider-openai`).

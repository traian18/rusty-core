# rusty-provider-openai-responses

Model backend for the OpenAI Responses API (`{base_url}/responses`), a different wire shape
from Chat Completions. It is also the transport the Codex subscription adapter builds on.
Integration id: `openai-responses`.

## Why it exists

A provider's wire format is the only thing that differs between model APIs; streaming, retries,
timeouts, circuit breaking, cost reporting and tool-call assembly are the same everywhere. So
each provider is a thin crate that implements `ModelClient` for one API, and
`rusty-model-backend` wraps it in `GenericModelBackend` to get everything else once. Adding a
provider means writing its request and response mapping and nothing more.

The Responses API models a conversation as typed input and output *items* (messages, function
calls, function call outputs) rather than chat messages, and reports completion with terminal
events instead of a `[DONE]` sentinel, so it needs its own mapping.

## How it works

`OpenAiResponsesClient::stream` is three steps: `build_body`, `send_request`, `read_response`.

- **Request** (`client/request.rs`, `wire/`): the conversation becomes input items; tools become
  function tools; the system prompt is an input message, or `instructions` for the ChatGPT
  subscription endpoint (which also rejects the output-budget and temperature knobs).
- **Response** (`wire/sse/frame.rs`): each frame is dispatched on its JSON `type`: text and
  reasoning deltas, a function call added, argument deltas, a function call done, and the
  terminal events `response.completed`, `response.incomplete` and `response.failed`. The stream is
  complete only after a terminal event.
- **Rate limits** (`client/response.rs`): a ChatGPT usage-limit 429 body says when the limit
  resets; that becomes the retry delay.

## Key types and concepts

- **`OpenAiResponsesClient`, `OpenAiResponsesConfig`**: client and settings; `with_auth` and
  `with_chatgpt_auth` attach provider authentication.
- **`ResponsesInputItem` and its content parts**: the typed items sent as input.
- **`OpenAiResponsesSseParser`**: the incremental parser.
- **Terminal events**: a response ends `completed` (stop reason `stop`, or `tool_calls` if a
  function was called), `incomplete` (`length`), or `failed` (an error).

## A worked example

A function call arrives as three frames:

```text
data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"fs.read"}}
data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\":\"a.rs\"}"}
data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","arguments":"{\"path\":\"a.rs\"}"}}
```

They produce `ToolCallStarted`, `ToolCallDelta` and `ToolCallCompleted`, and the final
`response.completed` ends the run with stop reason `tool_calls`.

## Public API

- `OpenAiResponsesBackend`, `OpenAiResponsesFactory`, `OpenAiResponsesConfig`.
- `client::OpenAiResponsesClient`, `wire` (items, conversion, `OpenAiResponsesSseParser`), `usage`.

## Errors

`response.failed` and top-level `error` frames become `BackendError` with the API's code (the
subscription backend nests them under `error`); a stream that ends before a terminal event is
`StreamInterrupted`; a 429 is `RateLimited`, with the reset time when the body gives one.

## Design decisions

- **Terminal events, not a sentinel.** Completion requires a terminal frame, so a cut connection
  is never mistaken for success.
- **Subscription cost is unknown.** For the ChatGPT subscription the reported cost is zeroed
  rather than invented.
- **No event log in the parser.** It returns events and keeps no copy; the earlier internal
  copy was never read and has been removed.

## Testing

`src/wire/tests.rs` covers conversion and the parser; `src/client/tests.rs` covers the usage-limit
reset and `Retry-After`. Run `cargo test -p rusty-provider-openai-responses`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-runtime`
- **Used by:** `rusty-provider-codex`, `rusty-provider-github-copilot`

## Must never

- Execute tools: a model response only *proposes* tool calls; the harness authorizes and runs them.
- Implement retries, circuit breaking or timeouts of its own; `GenericModelBackend` owns those.
- Depend on the engine, the runtime's session types, or another provider's internals (except the
  OpenAI-shaped providers, which share `rusty-provider-openai`).

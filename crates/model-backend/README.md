# rusty-model-backend

The provider-neutral model vocabulary (`ModelRequest`, `ModelEvent`, `ModelError`, the
`ModelClient` trait) and `GenericModelBackend`, which turns any `ModelClient` into an
`ExecutionBackend` for the agent runtime: streaming, tool-call assembly, retries with backoff and
jitter under a shared deadline, idle timeouts, a circuit breaker, and usage and cost reporting.

## Why it exists

Every model provider has its own request and response shapes, but the runtime wants one thing:
an `ExecutionBackend` that streams events and ends with a result. Putting the shared behavior here
means a provider crate only translates formats, and a fix to retry or timeout behavior reaches
every provider at once. The crate also owns the neutral vocabulary the providers speak, so a
provider never needs to know another provider or the runtime's internals.

## How it works

1. The runtime calls `ExecutionBackend::execute` with an `ExecutionRequest` (from
   `rusty-protocol`).
2. `GenericModelBackend` converts it into a `ModelRequest`, checks the client's
   `ModelCapabilities` (an image-bearing request against a client without images is refused
   before any network call), and calls `ModelClient::stream`.
3. The client sends `ModelEvent`s through a `ModelEventSender` as they arrive: text and reasoning
   deltas, tool calls started, extended and completed, usage updates, and finally `Completed`.
4. The backend forwards them as execution events. A failure is classified: rate limits and
   timeouts retry with backoff and jitter within a shared deadline (honoring `Retry-After`);
   repeated failures open the circuit breaker so a dead provider fails fast instead of stalling
   every run; an idle stream is cut after the idle timeout.

## Key types and concepts

- **`ModelClient`**: the trait a provider implements: `capabilities()` and
  `stream(request, events, cancel)`.
- **`ModelRequest`, `ModelCapabilities`**: what is asked (messages, tools, limits, reasoning
  effort, response format, provider options, cache key) and what a client supports (streaming,
  reasoning, tool calls, parallel tool calls, images, structured output).
- **`ModelEvent`, `ModelResult`**: the stream and its end (stop reason, usage, cost).
- **`ModelError`**: `Protocol`, `StreamInterrupted`, `BackendError { message, code }`,
  `InvalidRequest`, `RateLimited { retry_after }`, `Timeout`, `Cancelled`.
- **`InferenceAuth`**: supplies request headers (such as a refreshed OAuth token) for a body.
- **`GenericModelBackend`, `RecoveryPolicy`**: the backend and the retry and circuit settings.
- **`merge_provider_options`, `parse_retry_after`**: shared helpers for provider-specific
  request knobs and for `Retry-After` headers.

## A worked example

A provider crate plugs in like this:

```rust
let client = AnthropicClient::new(config);      // implements ModelClient
let backend = GenericModelBackend::new(Arc::new(client));   // or new_with_recovery(.., policy)
// `backend` is an ExecutionBackend: the runtime streams text, tool calls and usage from it, and a
// 429 carrying `Retry-After: 2` is retried after two seconds without the provider knowing.
```

When failures keep coming, the circuit opens: further requests fail at once with a clear error
until the cool-down passes, instead of each run waiting out its own timeouts.

## Public API

- `ModelClient`, `ModelEventSender`, `send_event`, `ModelEvent`, `ModelResult`, `ModelError`.
- `ModelRequest`, `ModelCapabilities`.
- `GenericModelBackend`, `RecoveryPolicy`.
- `auth::InferenceAuth`, `merge_provider_options`, `parse_retry_after`.
- `testing` (feature `testing`): fake model clients and a contract suite for backend
  implementors.

## Errors

`ModelError` is the only error type. `RateLimited` and `Timeout` are retried; `InvalidRequest`,
`Protocol` and `BackendError` with a non-retryable code are not; `Cancelled` propagates at once.
When retries are exhausted the last error is returned unchanged so the caller sees the real cause.

## Design decisions

- **One backend for all providers.** Divergent retry behavior between providers is a bug source;
  keeping it in one place removes it.
- **Capabilities are checked up front.** A request the client cannot serve fails before spending
  a network round trip.
- **A circuit breaker, not just retries.** A provider outage should not make every concurrent run
  spend its full timeout budget.
- **The breaker's lock survives a panic elsewhere.** A poisoned lock is still used, since the
  state cannot be left half-updated.

## Testing

`src/backend/tests.rs` covers streaming, retries, the circuit breaker and cancellation against
fake clients. `tests/contract.rs` is the contract suite every `ModelClient` should pass; provider
crates run it against their own client. Run `cargo test -p rusty-model-backend`.

## In the workspace

- **Depends on:** `rusty-protocol`, `rusty-runtime`
- **Used by:** `rusty-extension-api`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-gemini`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-provider-openai-compatible`, `rusty-provider-openai-responses`

## Must never

- Know any provider's wire format or endpoint.
- Execute tools or touch the workspace.
- Depend on the engine or on session management; it needs only `rusty-protocol` and the runtime's
  backend trait.

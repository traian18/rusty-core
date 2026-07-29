# Phase 3 — Generic Model Backend

**Goal (spec Section 71):** `ModelClient` + `GenericModelBackend` implemented, wired to one real provider (Anthropic), with real streaming, tool calls, and usage normalization — replacing the fake backend for at least one session type.
**Depends on:** Phase 2 complete.
**Crates touched:** `harness-model`, `harness-generic-backend`, `harness-integration-anthropic`.

**Locked decision reminder:** Anthropic is the first real provider (confirmed with user). Do not implement OpenAI/Gemini/others in this phase — spec Section 71 explicitly says "choose one API provider first... do not implement every provider."

---

## Tasks

### Task 3.1 — `ModelClient` trait and model-level protocol types
- **Files:** `crates/harness-model/src/client.rs`, `crates/harness-model/src/request.rs`, `crates/harness-model/src/events.rs`
- **Description:** Define `#[async_trait] pub trait ModelClient` (spec Section 13: `capabilities()`, `stream(request, events, cancel)`), `ModelCapabilities`, `ModelRequest` (system prompt, messages, tool definitions, model id, generation params), `ModelEventSink`/normalized model-level streaming events (text delta, tool-call delta/complete, usage, stop reason), `ModelResult`, `ModelError`. Keep this layer provider-agnostic — it is one level below `ExecutionBackend` and specifically models "raw model API" semantics (spec Section 13 diagram).
- **Acceptance criteria:** trait is `dyn`-compatible (`async-trait`); types compile independent of any provider crate.
- **Effort:** M
- **Depends on:** Phase 2 complete

### Task 3.2 — `GenericModelBackend`
- **Files:** `crates/harness-generic-backend/src/backend.rs`, `crates/harness-generic-backend/src/loop.rs`
- **Description:** Implement `GenericModelBackend` as an `ExecutionBackend` (the trait from Phase 2 Task 2.6) that owns the harness-controlled agent loop for any `Arc<dyn ModelClient>` (spec Section 13). Responsibilities: turn an `ExecutionRequest`/`ExecutionContext` into a `ModelRequest`, call `ModelClient::stream`, translate `ModelEvent`s into normalized `ExecutionEvent`s (the type from Phase 1 Task 1.5) on the `ExecutionEventSink`, handle the multi-turn loop when the model requests tool calls (i.e. `GenericModelBackend` itself does **not** execute tools — it emits `ExecutionEvent::ToolCallRequested` and expects the runtime/`AgentRunner` to drive tool execution and feed results back via a subsequent `ExecutionRequest`, keeping the actual tool-loop control in `harness-core`/`harness-runtime`, not the backend).
- **Acceptance criteria:** with a `FakeModelClient` (Task 3.3), `GenericModelBackend::execute` produces the same class of event stream that `FakeBackend` did in Phase 2, proving `AgentRunner` needs zero changes to consume a real-shaped backend.
- **Effort:** L
- **Depends on:** Task 3.1

### Task 3.3 — `FakeModelClient` and generic-backend contract tests
- **Files:** `crates/harness-generic-backend/tests/contract.rs`, `crates/harness-generic-backend/src/testing/fake_model_client.rs`
- **Description:** A scripted `ModelClient` fake, plus the first pass of the **backend contract test suite** (spec Section 68.6): streaming ordering, cancellation, completion, usage behavior, tool-event normalization, error normalization. Structure this as a reusable `pub fn run_backend_contract_suite(backend: Arc<dyn ExecutionBackend>)` helper so Phase 10's Claude Code/Codex backends can reuse it verbatim.
- **Acceptance criteria:** `GenericModelBackend` passes the full contract suite against `FakeModelClient`.
- **Effort:** M
- **Depends on:** Task 3.2

### Task 3.4 — Anthropic `ModelClient` implementation
- **Files:** `crates/integrations/anthropic/src/lib.rs`, `crates/integrations/anthropic/src/client.rs`, `crates/integrations/anthropic/src/wire.rs` (request/response JSON shapes), `crates/integrations/anthropic/src/config.rs`
- **Description:** Implement `ModelClient` for the Anthropic Messages API. Add `reqwest` (with `stream` feature) to this crate only — **not** to `harness-model`/`harness-core`/`harness-runtime` (spec Section 66 invariant: provider HTTP clients never leak inward). Streaming uses Anthropic's SSE (`text/event-stream`) responses; parse via `reqwest`'s byte stream + a small local SSE line-parser, or the `eventsource-stream` crate (evaluate at implementation time; either is acceptable, this is an implementation detail fully contained in this crate). `config.rs` holds `AnthropicConfig` (API key, base URL, default model) — the API key must never be logged or placed in any `AgentEvent`/`SessionEvent` (spec Section 62).
- **Acceptance criteria:** unit tests cover wire-format parsing (request serialization, SSE response parsing) using recorded fixture payloads (no live network calls in CI); a manual/local-only integration test (feature-gated, e.g. `#[ignore]` or a `manual-network-tests` feature) can hit the real API when a key is present.
- **Effort:** L
- **Depends on:** Task 3.1

### Task 3.5 — Anthropic usage/cost normalization
- **Files:** `crates/integrations/anthropic/src/usage.rs`
- **Description:** Map Anthropic's reported usage fields (input/output/cache-read/cache-write tokens) into `ModelUsage` (Phase 1 Task 1.4), leaving `reasoning_tokens: None` if not exposed. Compute `Cost` with `CostSource::Calculated` (rate table per model, kept in this crate) unless Anthropic starts reporting exact billed cost, in which case `CostSource::ProviderReported`.
- **Acceptance criteria:** a fixture-based unit test maps a real recorded Anthropic usage payload to the exact expected `ModelUsage`/`Cost`.
- **Effort:** M
- **Depends on:** Task 3.4

### Task 3.6 — `AnthropicBackend` composition and registration
- **Files:** `crates/integrations/anthropic/src/lib.rs`
- **Description:** Compose `GenericModelBackend::new(Arc::new(AnthropicClient::new(config)))` behind a small `AnthropicBackend` constructor so call sites match spec Section 12's example (`AnthropicBackend::new(config_a)`), even though internally it's just a configured `GenericModelBackend`. Implement an `IntegrationFactory` (spec Section 16) for registry-based construction, in addition to direct injection.
- **Acceptance criteria:** both `harness.session().backend(Arc::new(AnthropicBackend::new(config)))` and `harness.session().integration("anthropic", config)` construct a working session (spec Sections 12, 16).
- **Effort:** M
- **Depends on:** Tasks 3.2, 3.4, 3.5

### Task 3.7 — End-to-end Anthropic session test
- **Files:** `crates/integrations/anthropic/tests/session_e2e.rs`
- **Description:** Using recorded fixture SSE responses (not live network), run a full session through `harness-engine`'s builder (Phase 2 Task 2.9) with `AnthropicBackend`, asserting the resulting `SessionEvent` stream, final transcript, and usage records match expectations.
- **Acceptance criteria:** test passes deterministically in CI with zero network access.
- **Effort:** M
- **Depends on:** Task 3.6

---

## Testing (this phase)

- Backend contract suite (Task 3.3) established and reused going forward.
- Fixture-based (non-network) tests for Anthropic wire format and usage mapping.
- No live-network tests in CI; optionally gate a manual suite behind a feature flag / `#[ignore]` for local verification with a real API key.

## Exit criteria

- `GenericModelBackend` passes the contract suite against both `FakeModelClient` and the real `AnthropicClient` (via fixtures).
- A session bound to `AnthropicBackend` streams real (fixture-replayed) text, handles a tool-call round trip, and reports usage — spec Section 71 Phase 3 goal.
- No Anthropic-specific type or string leaks past `crates/integrations/anthropic`.

## Trade-offs / open decisions

- **SSE parsing approach:** hand-rolled minimal parser vs. an SSE crate (`eventsource-stream`/`reqwest-eventsource`) — left as an implementation-time choice since it's fully contained within `harness-integration-anthropic` and doesn't affect any other crate.
- **Cost calculation:** Anthropic's public API does not (as of this spec's writing) return exact billed cost, so `CostSource::Calculated` with an internally maintained per-model rate table is assumed; revisit if Anthropic later exposes provider-reported cost.

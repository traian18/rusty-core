# Rusty Harness production SDK plan

Status: In progress — Foundation increment scaffolded (see §0 below)
Prepared: 2026-08-05  
Last updated: 2026-08-05
Repository: `rusty-core`  
Primary goal: turn the harness into a production-ready, application-agnostic agent runtime that can be embedded or consumed from TypeScript, Rust, and Java without assumptions about an IDE, UI framework, or host process.

## 0. Implementation status (read this first)

This section tracks what actually exists in the repository against the plan
below. Keep it current — do not let the rest of this document read as done
work that hasn't landed.

### Done

- **`sdk/rust` (`rusty-harness-sdk`)** — a facade crate over `harness-engine`
  exposing `Client`/`ClientBuilder`, `Session`, `EventStream`, and curated
  `protocol`/`providers`/`integration`/`store` re-export modules. Added to
  the root workspace `Cargo.toml`. Not yet verified with `cargo build`/`cargo
  test` in this environment (no working `cargo`); review signatures against
  `crates/harness-engine` before depending on it, and run the workspace
  build/test/clippy gates from the README before merging.
- **`sdk/typescript` (`@rusty/harness-sdk`)** — hand-written source (types,
  errors, a `Transport` interface, a `StdioSidecarTransport`, `HarnessClient`,
  `HarnessSession`) matching the current wire protocol. Source-only: not
  built, type-checked, or published in this change.
- **`schema/protocol-v1.schema.json`** — hand-authored JSON Schema mirroring
  `harness-protocol`'s current Rust types, including the externally-tagged
  enum representation and its forward-compatibility limitations.
- **`schema/compatibility-policy.md`** — explicit statement that protocol v1
  is currently a build tag, not a compatibility guarantee, plus the concrete
  items required before it can be called SDK-002-compliant.
- **`docs/product-scope-v1.md`** — SDK-000 capability classification
  (required/experimental/deferred/unsupported) grounded in direct source
  inspection and `upgrade_rusty.md`, not aspiration.
- **`sdk/README.md`** — index and explicit "honesty about maturity" section
  linking every SDK back to the real engine gaps.

### Not done (do not assume otherwise)

- `sdk/java` does not exist (Phase 3, SDK-302) — intentionally deferred per
  §17 ("Java starts after the protocol and reference behavior stabilize").
- No codegen: the Rust SDK, TypeScript SDK, and JSON Schema are three
  independently hand-maintained mirrors of `harness-protocol`. SDK-201 (one
  source of truth + drift-detecting CI) is unstarted.
- No typed RPC errors, no `ListSessions`/session-rehydrate RPC, no
  `steer`/`follow_up`/`close_session` operations, no idempotent command IDs
  (SDK-101–SDK-103, SDK-200, SDK-202–SDK-203) — these require engine/runtime
  changes, not just SDK wrapping, and are unstarted.
- The underlying engine gaps in `upgrade_rusty.md` P0 (multi-turn
  reliability RST-002/003, truthful failure status RST-004, snapshot/replay
  RST-006–RST-009) are **unstarted**. The SDKs added in this pass wrap the
  engine as it exists today; they do not fix these gaps, and their own
  documentation says so.
- No conformance suite, no Java SDK, no packaging/release automation, no
  security/observability work (Phases 4, 6–9) has started.

### Recommended next increment

Per §17's "Foundation" and "Usable alpha" increments: (1) get `cargo
build`/`test`/`clippy` green for `sdk/rust` and fix any signature drift found
by the compiler; (2) `npm install && npm run build` for `sdk/typescript` and
fix any type errors; (3) then prioritize `upgrade_rusty.md` RST-002 (verify
or fix multi-turn reliability) before investing further in SDK ergonomics —
an SDK around a one-shot session is not a usable alpha.

---

## 1. Product outcome

Rusty should be distributable as both:

1. An embeddable Rust library for native applications.
2. A managed local sidecar for desktop applications, IDEs, CLIs, and automation.
3. A long-running daemon for trusted local or remote clients.

All three forms must expose the same session, run, event, permission, persistence, provider, tool, and recovery semantics.

The project is ready for a stable SDK release when an independent application can:

- discover server and model capabilities;
- create, inspect, list, restore, and close durable sessions;
- submit prompts, attachments, steering messages, and queued follow-ups;
- stream ordered events and reconnect without corrupting client state;
- resolve permissions and cancel active work;
- switch supported models and reasoning settings;
- inspect transcript, usage, context, tools, and subagents;
- survive client or daemon restarts;
- integrate through documented Rust, TypeScript, or Java APIs;
- run against a versioned protocol with a published compatibility policy;
- receive typed, actionable, redacted errors;
- deploy signed artifacts with a documented security model.

Replacing Pi in an existing IDE is one useful acceptance test, not the architecture or public API boundary.

## 2. Product principles

- **Application agnostic:** no public type refers to editor tabs, panels, commands, or a particular UI.
- **One behavioral contract:** embedded and wire clients observe equivalent operations and events.
- **Protocol first:** the wire schema is a supported product contract, not an internal Serde representation.
- **SDK parity:** Rust, TypeScript, and Java expose the same core feature set and terminology.
- **Host-owned policy:** workspace authorization, credentials, tools, storage, and permissions remain configurable by the host.
- **Durable by design:** persistence, ordering, replay, idempotency, and migration behavior are specified before a 1.0 release.
- **Secure defaults:** local-only listeners, explicit workspace roots, secret redaction, bounded resources, and deny/ask tool policy.
- **Additive evolution:** clients tolerate unknown optional fields and events; breaking changes require a protocol major version.
- **No hidden side effects:** SDK convenience layers may retry reads, but mutating operations require command idempotency.

## 3. Supported integration modes

```text
                         Rust application
                               |
                         sdk/rust (embedded)
                               |
                         harness-engine
                               |
                        shared agent core
                               |
       +-----------------------+-----------------------+
       |                       |                       |
   providers                 tools               session store
       |
    harnessd
       |
  versioned RPC protocol
       |
  +----+----------------------+----------------------+
  |                           |                      |
sdk/typescript             sdk/java          custom wire clients
  |                           |
desktop / server / CLI     JVM / Android* / server
```

`* Android support is not a version 1 commitment until process, filesystem, and networking constraints are validated.

The SDKs support two client styles:

- **Managed sidecar:** the SDK starts and supervises `harnessd --stdio`.
- **Connected client:** the SDK connects to an already-running daemon through IPC or authenticated WebSocket.

Only the Rust SDK supports direct in-process embedding in version 1. Native FFI, JNI, N-API, and WebAssembly should be deferred until real use cases justify their ABI and packaging cost.

## 4. Repository and SDK layout

Create a top-level `sdk/` directory and keep generated protocol artifacts close to their consumers:

```text
sdk/
  README.md                      [done]
  rust/                          [done: facade crate, alpha]
    Cargo.toml
    src/
    examples/
    tests/                       [not yet added]
  typescript/                    [done: source only, not built/published]
    package.json
    src/
      generated/                 [not yet added — no codegen yet]
      client/
      transports/
      sidecar/
    examples/
    test/                        [not yet added]
  java/                          [not started]
    build.gradle.kts
    settings.gradle.kts
    src/main/java/
    src/test/java/
    examples/
  conformance/                   [not started]
    fixtures/
    scenarios/
    README.md

schema/
  protocol-v1.schema.json        [done: hand-authored, not generated]
  compatibility-policy.md        [done]

docs/
  getting-started/               [not started]
  concepts/                      [not started]
  sdk/
    rust/                        [covered informally by sdk/rust/README.md]
    typescript/                  [covered informally by sdk/typescript/README.md]
    java/                        [not started]
  protocol/                      [not started — see schema/ instead for now]
  deployment/                    [not started]
  security/                      [not started]
  operations/                    [not started]
  migration/                     [not started]
  product-scope-v1.md            [done]
```

Note: the actual `sdk/typescript/src` layout implemented in this pass is
flat (`types.ts`, `errors.ts`, `transport.ts`, `client.ts`, `session.ts`,
`index.ts`) rather than the `client/`/`transports/`/`sidecar/` subdirectory
split shown above. Reorganize into that split as more transports and
sidecar-management logic are added (SDK-303); a flat layout for one
transport and one client class does not yet justify the subdirectories.

Naming decisions to lock before publishing:

- Rust crate: `rusty-harness-sdk` or `harness-sdk`. **Decided for this pass: `rusty-harness-sdk`** (matches the crate actually created).
- npm package: `@rusty/harness-sdk`. **Decided for this pass: `@rusty/harness-sdk`** (matches `sdk/typescript/package.json`, currently `private: true` and unpublished).
- Java coordinates: `io.rusty:harness-sdk`. Still undecided/unstarted.
- daemon executable: `harnessd`. Already the case (`apps/harnessd`).
- protocol name and public namespace: still undecided; `schema/protocol-v1.schema.json` uses the placeholder `$id` `https://rusty-core/schema/protocol-v1.schema.json`.

The Rust facade should be a workspace member. TypeScript and Java may use their native build systems but must participate in root CI and the same conformance suite.

## 5. Current baseline and principal gaps

| Area | Useful baseline | Production gap |
|---|---|---|
| Core | Deterministic state machine and async runtime separation | Multi-turn and busy-session semantics require a stable external contract |
| Rust embedding | `harness-engine::{Harness, SessionBuilder, SessionHandle}` | Public facade is thin and exposes incomplete controls/results |
| Daemon | `apps/harnessd` with stdio, IPC, and WebSocket | Incomplete lifecycle, discovery, restore, auth, health, and operations APIs |
| Protocol | Rust RPC types and hello/version handshake | No language-neutral source of truth, compatibility automation, or complete typed errors |
| Streaming | Typed envelopes and sequence fields | Commit ordering, gap recovery, replay, and client assembly require certification |
| Persistence | JSONL/SQLite abstractions and snapshots | Snapshot policy, migrations, trailing replay, and dependency restoration are incomplete |
| Providers | Multiple HTTP and subprocess integrations | Unified model catalog, credentials, capabilities, and switching are incomplete |
| Attachments | Attachment type exists | Public and daemon prompt paths do not consistently preserve content |
| Context | Context decorator and truncation foundation | Token-aware compaction, durable summaries, instructions, and overflow recovery are incomplete |
| Tools | Filesystem, shell, git, and web foundations | Output bounds, cancellation, security, edit ergonomics, and conformance need work |
| Extensions | Compile-time Rust extension API | Resource discovery, skills/templates, lifecycle hooks, and compatibility policy are incomplete |
| SDKs | Rust engine API, plus a new `sdk/rust` facade and `sdk/typescript` source (this pass) | No Java SDK, no codegen, no conformance suite, neither existing SDK is published or CI-verified yet |
| Operations | Tracing and usage foundations | Metrics, support bundles, redaction, auditability, and SLOs are not release-grade |

`upgrade_rusty.md` remains the detailed core gap audit. This plan defines the public product and delivery sequence.

## 6. Phase 0 — Lock contracts and product scope

### SDK-000: Define version 1 capability scope

**Status: done (initial draft).** See [`docs/product-scope-v1.md`](docs/product-scope-v1.md). Revise it as gaps close instead of treating it as final.

Create `docs/product-scope-v1.md` and classify features as:

- required for SDK 1.0;
- experimental and feature-negotiated;
- deferred;
- explicitly unsupported.

The decision must cover branching, skills, templates, custom tools, remote access, authentication, attachments, subagents, and provider credential flows.

### SDK-001: Write semantic specifications

**Status: not started.** `docs/product-scope-v1.md` classifies capabilities but does not yet specify the precise semantics listed below.

Specify independently of implementation:

- daemon, connection, session, run, and agent lifecycle;
- admission rules while a session is busy;
- prompt, steer, follow-up, continue, cancel-run, close-session, and shutdown;
- event ordering and durability;
- transcript representation;
- permission request lifetime;
- tool and model cancellation;
- attachment ownership and retention;
- context compaction;
- restore behavior and incompatible-state handling;
- multiple clients attached to one session.

### SDK-002: Establish API governance

**Status: not started.** `schema/compatibility-policy.md` covers a narrow slice (protocol versioning honesty) but not the full governance list below.

Add:

- semantic versioning rules for each SDK;
- protocol major/minor negotiation rules;
- supported server/SDK compatibility matrix;
- deprecation period and removal policy;
- schema change review checklist;
- public API review ownership;
- release cadence and long-term support policy;
- security disclosure and patch process.

Exit gate: core maintainers can answer how every public operation behaves without referring to a particular frontend.

## 7. Phase 1 — Stabilize the core lifecycle

**Status: not started.** This entire phase requires `harness-runtime`/`harness-core` changes, not SDK wrapping. See `upgrade_rusty.md` RST-002/RST-003 for the equivalent, more detailed engine-level plan — treat that document as authoritative for *how* to implement this phase; use this section only for the SDK-facing operation names.

### SDK-100: Make root sessions genuinely multi-turn

- Keep the root mailbox alive until explicit session shutdown.
- Return to an admissible state after completed, failed, or cancelled runs.
- Separate run completion from session termination.
- Use run-scoped result handles.
- Permit child agents to finish independently.
- Release all run resources before admitting conflicting work.

### SDK-101: Implement the command model

Expose equivalent operations in the engine and RPC:

- `prompt(input, options)`;
- `steer(input, options)`;
- `follow_up(input, queue_mode)`;
- `continue_run(options)`;
- `cancel_run(run_id?)`;
- `pause_run` and `resume_run`, or omit them from v1;
- `close_session(options)`;
- daemon `shutdown(options)`.

Document safe delivery points for steering and queue modes such as `one_at_a_time` and `all`.

### SDK-102: Add idempotent admission

Every mutation carries a client-generated `command_id`, optional expected revision, typed admission response, and correlated lifecycle events. Persist a bounded deduplication window so reconnecting clients cannot accidentally duplicate work.

### SDK-103: Separate state domains

Represent daemon, connection, session, run, and agent states separately. Add deterministic lifecycle tests for invalid transitions, races, cancellation, permission waits, and follow-up admission.

Exit gate: sequential prompts, steering, queued follow-ups, cancellation, failure recovery, and graceful close pass deterministic and runtime tests.

## 8. Phase 2 — Make the protocol a public product

**Status: partially started.** `schema/protocol-v1.schema.json` and `schema/compatibility-policy.md` document the *current* protocol honestly, but none of the SDK-2xx items below (typed errors, generated codecs, list/restore RPCs, reconnect guarantees, transport hardening) are implemented yet.

### SDK-200: Define protocol v1

Use explicit envelopes for requests, responses, and events with:

- request, command, session, run, agent, event, and trace IDs;
- protocol major/minor and SDK identity;
- feature/capability negotiation;
- stable JSON tags and field casing;
- structured error code, category, retryability, safe message, and details;
- maximum frame sizes and bounded payload rules;
- heartbeat, health, graceful close, and server-instance identity;
- unknown-event and unknown-field behavior;
- redaction rules.

### SDK-201: Establish a language-neutral source of truth

**Status: not started.** `schema/protocol-v1.schema.json` is hand-authored, not generated, and there is no drift-detection CI. `sdk/rust`, `sdk/typescript`, and the schema are three independent hand-maintained mirrors today — this is the gap SDK-201 exists to close.

Commit JSON Schema under `schema/`, generated from carefully annotated Rust protocol types or authored as the canonical definition. Generate DTOs/codecs for all SDKs and fail CI when generated files drift.

Generation must not leak runtime internals into SDK public models. Handwritten ergonomic client APIs wrap generated transport DTOs.

### SDK-202: Complete daemon APIs

Daemon:

- version, capabilities, health, readiness, diagnostics, shutdown.

Sessions:

- create, list, inspect, open/restore, close, archive/delete;
- prompt, steer, follow-up, continue, cancel;
- subscribe/unsubscribe/resume from cursor;
- transcript pagination;
- checkpoint, compact, metadata update;
- optional branch/fork when negotiated.

Providers and models:

- list providers/models/capabilities;
- provider health;
- credential-profile status and host-assisted auth flows;
- model selection and reasoning options.

Runtime:

- resolve permission;
- inspect tool policy and context state;
- list/inspect/cancel subagents;
- usage and cost projections.

### SDK-203: Guarantee reconnect semantics

Use one authoritative commit order:

1. assign session sequence;
2. persist durable event;
3. publish to subscribers;
4. update state projection.

Specify at-least-once delivery, ID deduplication, gap detection, cursor expiry, snapshot fallback, subscriber lag behavior, and complete durable events for any lost ephemeral deltas.

### SDK-204: Secure transports

- Stdio: parent-child trust boundary and stdout protocol isolation.
- IPC: peer identity where supported and restrictive socket permissions.
- WebSocket: loopback-only by default, TLS and authentication for non-loopback use.
- All transports: connection limits, request limits, timeouts, origin policy where applicable, and safe shutdown.

Exit gate: reference clients can reconstruct identical final state after disconnecting during every lifecycle state.

## 9. Phase 3 — Build the three SDKs

### SDK-300: Rust SDK

**Status: alpha, done in this pass.** `sdk/rust` (`rusty-harness-sdk`) exists as a workspace member wrapping `harness-engine`.

Delivered in this pass:

- an embedded `Client`/`ClientBuilder` (wraps `Harness`/`HarnessBuilder`);
- a `Session` wrapper around `SessionHandle` (send/cancel/resolve_permission/session_id, `Deref` to the raw handle for `snapshot()`/`context_inspection()`);
- an `EventStream` (`futures::Stream` + plain `next()`) wrapping the internal `broadcast::Receiver`, converting lag into a typed `SdkError::Lagged`;
- an `SdkError` covering engine, store, and stream-lag failures;
- curated `protocol`/`providers`/`integration`/`store` re-export modules so consumers need not add `harness-protocol`/`harness-runtime`/`harness-session-store` as direct dependencies;
- rustdoc on every public item plus a runnable `examples/basic_chat.rs`.

Not yet delivered (tracked, not silently dropped):

- no cancellation/deadline wrapper beyond what `HarnessError`/`cancel()` already provide;
- no host traits for credentials/workspace/storage/permissions beyond what `harness-engine` already exposes directly;
- no feature flags gating built-in providers/tools (the crate always depends on `harness-runtime` with `testing` enabled — revisit before any real release, this is a development convenience, not a production dependency choice);
- no semver/API-diff CI check (`cargo public-api` or similar);
- no `tests/` integration tests yet, and the crate has not been compiled in this environment (no working `cargo`) — run `cargo check -p rusty-harness-sdk` before relying on it.

### SDK-301: TypeScript SDK

**Status: alpha, source-only, done in this pass.** `sdk/typescript` (`@rusty/harness-sdk`) exists with a working design but has not been built or type-checked in this change.

Delivered in this pass:

- transport-independent `Transport` interface;
- `StdioSidecarTransport` (managed sidecar, spawns `harnessd --stdio`, newline-delimited JSON framing, stderr line forwarding);
- `HarnessClient` (mandatory `Hello` handshake, request/response correlation by ID, typed errors, per-request timeout);
- `HarnessSession` (ergonomic per-session wrapper, both callback `subscribe()` and `events()` async generator);
- typed errors (`HarnessRpcError`, `HarnessTimeoutError`, `HarnessVersionMismatchError`, `HarnessTransportClosedError`, `HarnessProtocolError`);
- hand-mirrored `types.ts` matching the current wire protocol's externally-tagged enum shapes.

Not yet delivered:

- WebSocket/IPC transports (only stdio sidecar exists);
- runtime frame validation against the schema (decoded JSON is cast, not validated);
- cursor-persistence helpers / automatic resubscribe-on-reconnect;
- delta assembly into complete assistant/reasoning/tool views (raw envelopes are exposed as-is);
- test fakes / in-memory mock transport;
- CJS policy decision (package is ESM-only, `"type": "module"`, today);
- Node version support matrix beyond `engines.node >= 18.18.0` in `package.json`;
- the package has never been run through `npm install`/`tsc`/`node --test` in this environment — do this before depending on it.

### SDK-302: Java SDK

**Status: not started**, per the deliberate sequencing in §17 ("Java starts after the protocol and reference behavior stabilize"). No `sdk/java` directory exists.

Do not claim Android support until a separate compatibility suite passes.

### SDK-303: Shared sidecar management behavior

**Status: partially started for TypeScript only.** `StdioSidecarTransport` resolves an explicit binary path, spawns without a shell, and forwards stderr lines, but does not yet: distinguish clean exit / crash / corruption / version mismatch as distinct typed states, terminate process trees, use bounded restart backoff, or avoid replaying uncertain mutations without command idempotency (idempotent command IDs do not exist yet — SDK-102). No shared `sdk/conformance` specification exists yet to keep a future Java sidecar consistent with this one.

### SDK-304: SDK API parity matrix

**Status: not started.** No `docs/sdk/feature-matrix.md` exists yet; `docs/product-scope-v1.md` covers protocol-level capability classification but not per-language API/doc/test/release status.

Exit gate: minimal standalone programs in all three languages create a session, stream output, handle a permission, cancel a run, reconnect, restore the session, and shut down cleanly.

## 10. Phase 4 — Trustworthy persistence and recovery

**Status: not started.** This phase is almost entirely `upgrade_rusty.md` RST-006–RST-010 territory (engine/runtime work), not SDK work. The SDKs added in this pass call `restore_session`/`list_sessions` as they exist today and document the gap in their own READMEs; they do not implement any of SDK-400–SDK-404.

(See `upgrade_rusty.md` Phase 3 for the detailed, already-planned engine-side implementation approach for this section; treat that document as the working plan and this section as the SDK-facing restatement.)

## 11. Phase 5 — Production agent behavior

**Status: not started** at the SDK layer; partially planned at the engine layer in `upgrade_rusty.md` (RST-011–RST-022, Phases 4/6 there). No SDK work should begin here until the underlying engine capability exists — wrapping a nonexistent model registry or attachment path would create an API that lies about what the engine can do.

## 12. Phase 6 — Extensions without frontend assumptions

**Status: not started.** `harness-extension-api` remains compile-time-Rust-only by design (confirmed by direct inspection of its module doc). No skills/prompt-template discovery, no daemon-side tool/plugin registry, and no external tool protocol exist yet.

## 13. Phase 7 — Documentation and developer experience

**Status: minimally started.** `sdk/README.md`, `sdk/rust/README.md`, and `sdk/typescript/README.md` provide quick starts and honest scope statements, but there is no docs site, no per-language generated API reference, no protocol reference beyond the raw schema file, and none of the required runnable-example categories beyond "create and multi-turn chat" / "streaming" exist for either SDK yet.

## 14. Phase 8 — Testing, security, and operations

**Status: not started.** No `sdk/conformance` suite, no fault-injection tests, no security test suite, no soak tests, no packaging tests exist yet.

## 15. Phase 9 — Packaging and releases

**Status: not started.** Neither SDK is published (`sdk/rust/Cargo.toml` has `publish = false`; `sdk/typescript/package.json` has `"private": true`). No release automation, signing, SBOM, or install tests exist yet.

## 16. Pi replacement as an optional validation track

Unchanged from the prior revision of this document — still not started, still explicitly scoped as an adapter-boundary concern, not a core API concern.

Pi compatibility must not shape the native APIs. For consumers migrating from Pi:

- inventory the application's actual Pi calls and events;
- map behaviors to native Rusty operations;
- capture sanitized behavioral traces;
- implement a temporary adapter outside the core;
- run parity scenarios against both runtimes;
- migrate the host to native Rusty models incrementally;
- remove the adapter after cutover.

Keep application-specific adapters in separate packages or repositories. Do not add editor concepts or Pi field names to the core protocol unless they represent a general agent-runtime concept.

## 17. Delivery order and dependencies

```text
Contract and governance
          |
Core lifecycle semantics
          |
Public protocol + persistence guarantees
          |
   +------+--------+
   |      |        |
 Rust    TS      Java SDK
   +------+--------+
          |
Shared conformance and docs
          |
Agent behavior hardening
          |
Security, packaging, release candidates
          |
        SDK 1.0
```

Recommended implementation increments:

1. **Foundation:** Phases 0–2, with protocol fixtures and a reference client. **→ Partially done in this pass:** `docs/product-scope-v1.md`, `schema/protocol-v1.schema.json`, `schema/compatibility-policy.md`, and `sdk/rust` + `sdk/typescript` as reference clients exist. SDK-001/SDK-002/SDK-201 governance and generated-schema work remain.
2. **Usable alpha:** Rust and TypeScript SDKs, multi-turn sessions, streaming, typed errors. **→ SDK shells exist; multi-turn reliability (RST-002) and typed RPC errors (SDK-200) do not. Do not call this increment complete until those two land.**
3. **Durable alpha:** restore, replay, daemon discovery, attachments, model registry. Not started.
4. **Cross-language beta:** Java SDK and shared conformance passing. Not started.
5. **Production beta:** context, tools, permissions, security, observability, installers. Not started.
6. **Release candidate:** complete docs, compatibility testing, signing, soak and crash certification. Not started.
7. **1.0:** no unresolved critical security/data-loss issues and all gates below satisfied. Not started.

Java starts after the protocol and reference behavior stabilize; it should not be forced to discover protocol design flaws already found by Rust and TypeScript.

## 18. Production release gates

Unchanged from the prior revision — none of these gates are met yet. Re-verify each explicitly as work lands rather than assuming partial progress implies partial credit on a gate; these are pass/fail, not percentage, checks.

### API and compatibility

- Protocol schema is versioned and published.
- Rust, TypeScript, and Java feature matrix has no unexplained core gaps.
- Supported SDK/server version combinations pass conformance.
- Unknown additive fields/events do not break clients.
- Breaking-change detection runs in CI.

### Correctness and durability

- Multi-turn, steer, follow-up, cancel, close, and restore pass.
- Event ordering, replay, deduplication, and cursor fallback pass under fault injection.
- No acknowledged mutation is lost within the documented durability mode.
- No uncertain retry duplicates a command.
- Corrupt or incompatible sessions fail safely with repair guidance.

### Security

- Workspace boundaries and symlink escapes are tested.
- Tool policies and permission resolution are auditable.
- Secrets do not appear in events, snapshots, logs, or support bundles.
- Remote transport requires documented authentication and encryption.
- Artifacts are signed and ship with SBOM/provenance.

### Reliability and operations

- Process-tree cancellation works on supported platforms.
- Soak tests show bounded memory, tasks, descriptors, and queues.
- Daemon crash/restart and client reconnect meet documented recovery targets.
- Health/readiness and diagnostics work without exposing secrets.
- Provider outages and throttling yield typed, retryable behavior.

### Developer experience

- Fresh projects in all three languages pass quick starts from published packages.
- Every public API has reference documentation.
- Examples and fake backend run without paid credentials.
- Deployment, upgrade, incompatibility, and troubleshooting guides are complete.

## 19. Definition of done

The core can be called a production SDK platform—and a credible Pi replacement for hosts that need its supported feature set—when:

- `sdk/rust`, `sdk/typescript`, and `sdk/java` are published and supported;
- applications can choose embedded, managed-sidecar, or connected-daemon deployment without changing agent semantics;
- the protocol and durable state have explicit compatibility and migration policies;
- SDKs pass one shared cross-language behavioral suite;
- session lifecycle, context, persistence, providers, tools, permissions, and recovery meet the release gates;
- documentation lets an unfamiliar developer integrate without reading internal crates;
- application-specific migrations require adapters only at the application boundary;
- signed artifacts, security documentation, observability, and support procedures are in place.

Until those conditions hold, releases should be labeled alpha/beta and avoid compatibility promises that the project cannot yet maintain. **As of this update: `sdk/rust` and `sdk/typescript` exist as alpha-quality scaffolding over the current engine and protocol; every other bullet in this section remains open.**

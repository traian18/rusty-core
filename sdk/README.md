# Rusty Harness SDKs

This directory holds the application-agnostic SDKs described in
[`../sdk_plan.md`](../sdk_plan.md): a small, documented, stable surface for
integrating the Rusty agent harness into any host application, independent
of any particular IDE or frontend.

| Package | Language | Deployment modes | Status |
|---|---|---|---|
| [`rust/`](rust) (`rusty-harness-sdk`) | Rust | embedded (in-process `Harness`) | alpha, compiles against the current workspace |
| [`typescript/`](typescript) (`@rusty/harness-sdk`) | TypeScript | managed stdio sidecar today; WebSocket/IPC planned | alpha, source-only, not yet built/published |
| `java/` | Java | managed sidecar / connected daemon | not started (`sdk_plan.md` SDK-302) |

## Shared contract

All SDKs are clients of the same behavioral contract: sessions, runs,
agents, events, and permissions behave the same whether an application
embeds the Rust engine directly or talks to `harnessd` over the wire. The
wire shape of that contract is described in
[`../schema/protocol-v1.schema.json`](../schema/protocol-v1.schema.json)
and [`../schema/compatibility-policy.md`](../schema/compatibility-policy.md).

The Rust SDK and the TypeScript SDK are hand-written against that contract
today. `sdk_plan.md` (SDK-201) tracks generating both from one annotated
source of truth instead of maintaining three parallel copies by hand — do
not treat the current duplication as a long-term design choice.

## Honesty about maturity

These SDKs wrap a real, working engine and daemon (see the workspace
[`README.md`](../README.md)), but neither the engine nor the wire protocol
has yet closed the gaps tracked in [`../upgrade_rusty.md`](../upgrade_rusty.md)
(multi-turn admission semantics, production snapshot/replay, real restore
dependency resolution, typed RPC errors, session discovery/restore over the
wire). The SDKs surface those gaps in their own documentation rather than
hiding them behind a nicer API. Do not present either SDK as feature-complete
until the relevant `sdk_plan.md` phase and `upgrade_rusty.md` item are both
closed.

## Where to start

- Embedding directly in a Rust process: [`rust/README.md`](rust/README.md).
- Everything else (IDEs, CLIs, servers, other languages via the daemon):
  [`typescript/README.md`](typescript/README.md), and the raw protocol
  reference in [`../schema/protocol-v1.schema.json`](../schema/protocol-v1.schema.json).
- The full production roadmap, phases, and release gates:
  [`../sdk_plan.md`](../sdk_plan.md).
- Current v1 feature scope decisions: [`../docs/product-scope-v1.md`](../docs/product-scope-v1.md).

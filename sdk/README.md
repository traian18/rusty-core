# Rusty Harness SDKs

This directory holds the application-agnostic SDKs: a small, documented, stable surface for
integrating the Rusty agent harness into any host application, independent
of any particular IDE or frontend.

| Package | Language | Deployment modes | Status |
|---|---|---|---|
| [`rust/`](rust) (`rusty-harness-sdk`) | Rust | embedded (in-process `Harness`) | alpha, compiles against the current workspace |
| [`typescript/`](typescript) (`@rusty/harness-sdk`) | TypeScript | managed stdio sidecar today; WebSocket/IPC planned | alpha, source-only, not yet built/published |
| `java/` | Java | managed sidecar / connected daemon | not started |

## Shared contract

All SDKs are clients of the same behavioral contract: sessions, runs,
agents, events, permissions, and [MCP server](../README.md#mcp-servers)
connections behave the same whether an application embeds the Rust engine
directly or talks to `harnessd` over the wire. The wire shape of that
contract is described in
[`../schema/protocol-v2.schema.json`](../schema/protocol-v2.schema.json).

The Rust SDK and the TypeScript SDK are hand-written against that contract
today. Generating both from one annotated source of truth would be better
than maintaining parallel copies by hand — do not treat the current
duplication as a long-term design choice.

## Honesty about maturity

These SDKs wrap a real, working engine and daemon (see the workspace
[`README.md`](../README.md)). The wire protocol has typed errors, admission results with command
ids, and session listing and restore, but the SDKs are still hand-written and alpha: neither is
published, and each lists what it does not yet do in its own documentation rather than hiding it
behind a nicer API. Do not present either SDK as feature-complete.

## Where to start

- Embedding directly in a Rust process: [`rust/README.md`](rust/README.md).
- Everything else (IDEs, CLIs, servers, other languages via the daemon):
  [`typescript/README.md`](typescript/README.md), and the raw protocol
  reference in [`../schema/protocol-v2.schema.json`](../schema/protocol-v2.schema.json).

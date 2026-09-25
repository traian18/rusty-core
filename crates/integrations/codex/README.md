# ChatGPT subscription inference

`codex` is an inference-only adapter. It sends HTTP Responses requests to
`https://chatgpt.com/backend-api/codex/responses` using the existing ChatGPT
subscription sign-in. It never launches `codex exec`, resumes a provider-owned
agent, or gives a provider an executor. The old CLI execution configuration is
rejected rather than silently honored.

The request carries the model, reasoning effort, instructions, conversation,
and the harness-approved function schemas. Returned function calls go through
`GenericModelBackend` and the runtime permission gate before any executor runs.
Results return as `function_call_output` with the provider's original call ID.
The subscription request uses `store: false`, `stream: true`, and omits API-only
output-budget and temperature parameters. Subscription cost remains unknown.

## Authentication

Credentials never pass through the frontend/model. Each request reloads
`$CODEX_HOME/auth.json` (default `~/.codex/auth.json`), with a macOS Keychain
fallback for the `Codex Auth` store. An explicit `auth_path` selects only that
file. Expiring access tokens refresh through OpenAI's OAuth token endpoint.
Refreshes are serialized across sessions in the process and persisted atomically
with private file permissions, retaining the selected ChatGPT account.
Missing/revoked credentials require signing in again; there is no API-key billing
fallback or CLI execution fallback. Authentication UI may still use the managed
CLI to log in, but task execution never does.

## Enforcement and future extensions

The IDE passes the active mode, enabled tool IDs, and allowed MCP server names.
The core filters advertised tools and checks authorization again at dispatch.
A provider response cannot override that policy. Adapters reporting
`backend_managed_tools` are rejected by both session construction and execution.
The Claude and Copilot integrations also use inference-only APIs. No managed
provider launches an agent CLI for task execution.

Future tool processing hooks belong between authorization and dispatch/result
return in the harness. A summarizer running another model must receive only the
approved tool result and no executor or wider permissions. The original result
and derived summary should have distinct provenance. Any hook that changes a tool name or arguments must pass authorization again
before dispatch. Delegated inference must inherit the run cancellation and budget
limits. No summarizer or model routing hook is implemented in this change.

## References and validation

The subscription endpoint/auth pattern was checked against OpenCode commit
`0f549842ee746e400b1f72516b0b2e292e267e2c`,
[`plugin/openai/codex.ts`](https://github.com/anomalyco/opencode/blob/0f549842ee746e400b1f72516b0b2e292e267e2c/packages/opencode/src/plugin/openai/codex.ts).
Credential storage is documented in [Codex authentication](https://developers.openai.com/codex/auth).

`cargo test -p harness-integration-codex` uses fake credentials and local HTTP
fixtures. It covers refresh/rotation, the Responses tool loop, allowed reads,
Plan-mode write denial, and ungranted web access denial. No live subscription
request is part of the test suite.

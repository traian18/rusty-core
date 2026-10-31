# rusty-provider-github-copilot

Inference through a **GitHub Copilot subscription** instead of an API key. It is an inference-only adapter: it never launches a vendor CLI or gives a provider an executor. Tools only ever run through the harness's own permission gate.

## Why it exists

A provider's wire format is the only thing that differs between model APIs; streaming, retries,
timeouts, circuit breaking, cost reporting and tool-call assembly are the same everywhere. So
each provider is a thin crate that implements `ModelClient` for one API, and
`rusty-model-backend` wraps it in `GenericModelBackend` to get everything else once. Adding a
provider means writing its request and response mapping and nothing more.

Subscription access differs from API-key access in sign-in (OAuth device flow, token refresh), in which endpoints a model supports, and in what the provider reports about cost. Those differences are contained here.

## How it works

`github-copilot` now uses Copilot's model APIs rather than the Copilot agent CLI.
Models advertising `/v1/messages` use Anthropic Messages; otherwise models
advertising `/responses` use Responses, then `/chat/completions` use Chat
Completions. Without endpoint metadata, GPT-5 and later use Responses (except
GPT-5 mini) and other models use Chat Completions, matching OpenCode's routing.
Model responses only propose
tool calls. All tool authorization and execution belong to the harness.

Models are resolved against the direct API's catalog (`GET /models`, cached for
five minutes), using the same OAuth credential as inference. Rusty's device
login uses its own registered GitHub OAuth client and `read:user` scope. Every
catalog and inference request sends `Copilot-Integration-Id: copilot-developer-cli`.
Without that integration route, a valid custom OAuth credential receives a
legacy-only catalog and newer models are rejected. Rusty stores its direct
credential separately from Copilot CLI sign-in. `auto` picks the
configured default (`gpt-4.1`) only when the account has it, then Copilot's chat default and
fallback, then the first selectable model. A model the user chose is always sent
as-is; the catalog never rejects it. If Copilot itself answers
`model_not_supported`, that response is kept and annotated with the models the
direct API lists. The catalog's `supported_endpoints`
overrides the name-based routing when they disagree. The catalog's output-token
limit also caps requests. If the
catalog cannot be read, requests proceed unchecked and the API decides.

Rusty IDE injects an explicit `credentials_path` into every Copilot session,
pointing to its native device-login credential. On macOS the token lives in the
`rusty-copilot-inference` Keychain service; the file contains account metadata.
Other platforms use a native credential file (mode 0600 on Unix). Logout removes
this credential independently of CLI sign-in. Credential metadata includes the
public OAuth client ID; Rusty IDE requires a new sign-in if it belongs to another
app or an older sign-in without registration metadata. An explicit `credentials_path`
uses only that file, including for discovery.

For standalone core integrations without an explicit path, legacy token
resolution remains available: `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`,
then the selected account in `$COPILOT_HOME/config.json` (default
`~/.copilot/config.json`), including macOS's `copilot-cli` Keychain service.
No CLI is launched to retrieve tokens. A different selected account/enterprise
host fails rather than sending credentials to the wrong inference host.
Use `COPILOT_GH_HOST` or `github_host` for GitHub Enterprise Cloud `.ghe.com`.

Requests identify user/agent initiation and image inputs with Copilot's headers.
Both inference and `/models` send `X-GitHub-Api-Version: 2026-06-01`.
This header alone does not make the direct API expose the CLI's newer models.
The bearer token is never passed to the frontend or model. Legacy `binary_path`
and other CLI configuration fields are rejected.

Reference: OpenCode commit `c42ae0d56b6f86f8df39d451d6d2cfe6414b3928`,
`packages/opencode/src/plugin/github-copilot/copilot.ts` and `models.ts`.

## Key types and concepts

- **Auth** (`auth`, `credentials`): sign-in flow, stored credentials and token refresh; credentials never pass through the frontend or the model.
- **Backend and factory** (`backend`): the integration that routes each model to the right wire format.
- **Reused wire crates**: the Responses and Chat Completions transports of `rusty-provider-openai-responses` and `rusty-provider-openai` (and Anthropic's for Claude models on Copilot).

## A worked example

Routing decides the transport per model, not per request: a model that advertises the Messages endpoint uses the Anthropic wire, one that advertises Responses uses the Responses wire, and anything else uses Chat Completions. Tool calls come back as proposals; the runtime's permission gate decides whether they run.

## Public API

- The backend and integration factory the daemon registers, the configuration type, and the auth and credential helpers the sign-in flow uses.

## Errors

Authentication failures (no credential, expired and unrefreshable token) are reported as errors that tell the user to sign in again; provider failures are `ModelError`s from the reused clients.

## Design decisions

- **Inference only.** The subscription never runs an agent of its own, so permission and sandboxing stay with the harness.
- **Credentials are reloaded per request**, so a refreshed token is picked up without restarting.
- **Wire formats are reused, not copied.** Behavior shared with the API-key providers stays in one place.

## Testing

Unit tests beside the auth, credential and routing code cover token handling and endpoint selection. Run `cargo test -p rusty-provider-github-copilot`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-protocol`, `rusty-provider-anthropic`, `rusty-provider-openai`, `rusty-provider-openai-responses`, `rusty-runtime`
- **Used by:** `harness`, `harnessd`

## Must never

- Execute tools: a model response only *proposes* tool calls; the harness authorizes and runs them.
- Implement retries, circuit breaking or timeouts of its own; `GenericModelBackend` owns those.
- Depend on the engine, the runtime's session types, or another provider's internals (except the
  OpenAI-shaped providers, which share `rusty-provider-openai`).
- Write credentials anywhere but the dedicated credential store, or log them.

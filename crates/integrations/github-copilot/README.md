# Copilot subscription inference

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

# Copilot subscription inference

`github-copilot` now uses Copilot's model APIs rather than the Copilot agent CLI.
GPT-5 and later use Responses (except GPT-5 mini); other models use Chat
Completions, matching OpenCode's provider routing. Model responses only propose
tool calls. All tool authorization and execution belong to the harness.

Credentials are read from `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, or `GITHUB_TOKEN`,
then the selected account in `$COPILOT_HOME/config.json` (default
`~/.copilot/config.json`). File credentials and macOS's `copilot-cli` Keychain
service are supported. An explicit `credentials_path` uses only that file.
No CLI is launched to retrieve tokens. A different selected account/enterprise
host fails rather than sending credentials to the wrong inference host.
Use `COPILOT_GH_HOST` or `github_host` for GitHub Enterprise Cloud `.ghe.com`.

Requests identify user/agent initiation and image inputs with Copilot's headers.
The bearer token is never passed to the frontend or model. Legacy `binary_path`
and other CLI configuration fields are rejected.

Reference: OpenCode commit `0f549842ee746e400b1f72516b0b2e292e267e2c`,
`packages/opencode/src/plugin/github-copilot/copilot.ts` and provider routing in
`packages/opencode/src/provider/provider.ts`.

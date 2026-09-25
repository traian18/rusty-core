# Claude subscription inference

The existing `claude-code` integration ID now wraps the Anthropic Messages API
through `GenericModelBackend`. It never launches Claude's CLI or exposes Claude's
built-in tools. The IDE provides tool schemas and the harness owns authorization,
execution, and returning tool results, including for Build and Plan skills.
Legacy CLI arguments and permission flags are rejected during configuration.

Authentication loads `CLAUDE_CODE_OAUTH_TOKEN`, or the OAuth credential in
`$CLAUDE_CONFIG_DIR/.credentials.json` / `~/.claude/.credentials.json`. On macOS,
the default directory has a fallback to the `Claude Code-credentials` Keychain
service. An explicit `credentials_path` uses only that file. Expiring stored
tokens are refreshed and persisted privately and atomically. Tokens never cross
the frontend/model boundary. This adapter does not silently fall back to API-key
billing or a CLI when subscription authentication fails. Provider-side entitlement
restrictions still apply; a successful CLI login alone does not prove the token
is accepted for Messages inference.

The credential and HTTP tests use fixtures. The ignored live smoke test requires
an accessible local subscription credential and sends a single short request with
no files or tools. It is excluded from normal tests.

# harness-engine

The public entry point for embedding the harness. It composes everything else into something an application can use in a few lines: build a `Harness`, ask it for a `SessionBuilder`, choose an integration and tools, `start()`. Hosts such as the IDE, `harnessd`, the TUI and the SDK all go through this crate.

## What's inside

- `Harness` / `HarnessBuilder` — the integration registry, shared session manager, session store, profiles and orchestration config.
- `SessionBuilder` — one fluent API for backend or integration, workspace, tools, MCP servers, skills, execution policy, behavior profile, execution params and context providers (including `context_provider_with_backend` for providers that need the session's own backend).
- `profiles` — the host's behavior-profile registry, workspace profile loading and the built-in neutral profile every session starts under.
- `orchestration` — optional workflow runs on top of an engine session.
- `validation` — editor-facing validation of profile and workflow JSON that never fails: every problem comes back as an issue.
- `providers`, `tool_factory` — provider/credential/model-catalog types and construction of the built-in tools.

## In the workspace

- **Depends on:** `harness-context`, `harness-core`, `harness-protocol`, `harness-runtime`, `harness-session-store`, `harness-skills`, `harness-tool-filesystem`, `harness-tool-git`, `harness-tool-mcp`, `harness-tool-shell`, `harness-tool-skills`, `harness-tool-web`, `harness-tools`, `harness-workspace`
- **Used by:** `harness`, `harnessd`, `rusty-harness-sdk`

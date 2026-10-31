# rusty-engine

The assembly point of the core. It takes the pieces (a model backend, tools, a workspace, skills,
MCP servers, profiles, workflows) and returns something a host can use: a `Harness` that creates and
restores sessions, and a `SessionHandle` that talks to one. Every host (IDE, daemon, CLI, SDK) builds
the same way through this crate.

## Why it exists

The lower crates are deliberately independent: the runtime does not know which tools exist, the
tools do not know about sessions. Someone has to decide that a session gets the filesystem, shell,
git and web tools, the repository tools, skills, a context provider chain and a profile. Doing that in
one place keeps hosts thin and keeps the answer to "what does a session contain" in one file tree.

## How it works

1. `HarnessBuilder` collects integrations (model providers), a session store, a profile registry and
   an optional workflow configuration, and `build()` produces a `Harness`.
2. `Harness::session()` returns a `SessionBuilder`. Its options (`options`) set the backend or
   integration, tools, MCP servers, profiles, workspace, execution parameters, skills and
   orchestration; its tool sets (`toolsets`) choose between the plain, call-graph and
   repository-intelligence tool sets and wrap the backend with context providers.
3. `start()` creates the session through the session manager and returns a `SessionHandle`.
4. A stored session comes back through `Harness::restore_session*` (`restore`), supplied with the
   same kind of tool set.
5. Providers, credentials and model catalogs (`providers`, `harness/provider_ops`) turn a user's
   selection into a backend. `orchestration` registers and runs workflows on a handle; `validation`
   checks profile and workflow JSON for editors without ever failing.

## Key types and concepts

- **`Harness`**: the host-facing entry; owns the session manager, store and integrations.
- **`SessionBuilder` / `SessionHandle`**: configure and then talk to one session.
- **`ProfilesConfig`**: where behavior profiles come from and whether command evaluators are trusted.
- **`OrchestrationConfig`**: the workflow registry, store and schemas.
- **`ProviderKey`, `CredentialStore`, `ModelDescriptor`**: the provider and model catalog types.

## A worked example

```rust
let harness = Harness::builder()
    .register_integration(anthropic_factory)
    .session_store(store)
    .build()
    .await?;
let session = harness.session().backend(backend).workspace(workspace).start().await?;
```

## Public API

- `Harness`, `HarnessBuilder`, `SessionBuilder`, `SessionHandle`, `HarnessError`.
- `ProfilesConfig`, `OrchestrationConfig`, `OrchestrationRequest`, `McpServerConfig`, `SkillsConfig`.
- The provider and credential types; the re-exported workspace types hosts need.

## Errors

`HarnessError` covers configuration, provider, session and workflow failures with a message a host
can show. Validation never returns an error: every problem is an issue in its result.

## Design decisions

- **Thin hosts.** Hosts never assemble tools themselves.
- **Explicit re-exports.** The public surface is a list, not a glob, so adding an internal type does
  not widen it.
- **Impl blocks grouped by job** (options, tool sets, restore, providers) instead of one long file.

## Testing

Builder and validation tests run against fake backends; end-to-end session tests live in the daemon.
Run `cargo test -p rusty-engine`.

## In the workspace

- **Depends on:** `rusty-agent-core`, `rusty-code-analysis`, `rusty-context`, `rusty-protocol`, `rusty-repo-model`, `rusty-runtime`, `rusty-session-store`, `rusty-skills`, `rusty-tool-api`, `rusty-tool-code`, `rusty-tool-filesystem`, `rusty-tool-git`, `rusty-tool-mcp`, `rusty-tool-repo`, `rusty-tool-shell`, `rusty-tool-web`, `rusty-workflow-core`, `rusty-workflow-runtime`, `rusty-workspace`
- **Used by:** `harness`, `harnessd`, `rusty-harness-sdk`

## Must never

- Run a session itself: that is `rusty-runtime`.
- Hold provider secrets longer than a request needs them.
- Let a host skip the context and tool wiring a session needs.

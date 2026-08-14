# MCP server mode, MCP HTTP transport, and a filesystem skills system

## Context

The comparative assessment on the Desktop flags three gaps that block open-source launch. Two of them are real; one is already closed:

- **MCP client — already done.** `crates/tools/mcp` is a complete hand-rolled stdio client wired end-to-end (`SessionBuilder::mcp_server` → `McpToolExecutor` → `SimpleToolRegistry`, plus `McpServerSpec` on the `CreateSession` RPC). The assessment's "MCP client only, stdio-only" line describes what shipped in `8b35a34`.
- **MCP server mode — missing.** rusty-core can consume MCP servers but cannot present itself as one, so Claude Desktop / Cursor / VS Code cannot drive the engine without a custom SDK. This is P0 in the assessment.
- **MCP HTTP transport — missing.** `McpClient` hard-codes `ChildStdin`/`ChildStdout`, so hosted/remote MCP servers are unreachable.
- **Skills — do not exist at all.** Zero occurrences of "skill" in `crates/`, `apps/`, `sdk/`, or the README. The `Plugin` trait in `harness-extension-api` is compile-time by explicit design, which the assessment names as the adoption barrier. There is nothing to "extend" yet; this is greenfield.

Intended outcome: a user drops a directory on disk and gets a new agent capability with no recompile; an IDE points its MCP config at `harnessd` and drives real sessions; and MCP servers reachable over HTTP work the same as stdio ones.

The three workstreams are independent. Recommended order is **Skills → MCP HTTP → MCP server mode** (skills touch no wire format and de-risk the `ContextProvider` chaining; MCP server mode is the largest).

---

## Workstream 1 — Filesystem skills (`SKILL.md`)

### New crates

**`crates/harness-skills`** (`harness-skills`) — discovery, parsing, catalog, and the context provider.

```rust
pub struct Skill {
    pub name: String,          // from frontmatter; must match dir name
    pub description: String,   // the only thing always in the system prompt
    pub instructions: String,  // markdown body, loaded on demand
    pub allowed_tools: Vec<String>,
    pub dir: PathBuf,
    pub source: SkillSource,   // Workspace | User | Explicit
}

pub struct SkillCatalog { /* name -> Skill */ }

pub struct SkillsConfig {
    pub workspace_root: Option<PathBuf>,  // scans <root>/.rusty/skills/
    pub user_dir: bool,                   // scans ~/.rusty/skills/
    pub extra_roots: Vec<PathBuf>,
}
```

Discovery scans each root for `*/SKILL.md`. Later roots override earlier ones by `name` (workspace beats user), matching how `.rusty/` is already used as the project-local config dir alongside `.harness/`.

**Frontmatter parsing:** write a small flat-YAML frontmatter reader (~60 lines: `---` fence, `key: value`, `key: [a, b]`) rather than taking a YAML dependency. `serde_yaml` is archived and `deny.toml` sets `yanked = "deny"` with an existing advisory exception the project clearly wants to keep short. This also matches the crate's existing precedent of hand-rolling the MCP JSON-RPC client instead of pulling `rmcp`. Fields: `name`, `description` (required); `version`, `license`, `allowed-tools` (optional).

**`SkillsContextProvider`** implements `harness_context::ContextProvider` (from [provider.rs](crates/harness-context/src/provider.rs)) and appends only the *catalog* — one `name: description` line per skill — to `request.system_prompt`. That is the progressive-disclosure boundary: bodies never enter the prompt unless the model asks.

**`crates/tools/skills`** (`harness-tool-skills`) — two `ToolExecutor`s, following the `harness-tool-*` naming that `xtask check-deps` already bans from core:

- `skill.load { name }` → returns the full `instructions` body plus a listing of files bundled in the skill dir.
- `skill.read { name, path }` → reads a bundled file, path-scoped to that skill's dir. Needed because skill dirs live outside the workspace root, so `fs.read` cannot reach them. Reject `..` traversal and symlinks escaping the dir.

### Changes to existing code

- **`crates/harness-context/src/providers.rs`** — add `ChainedContextProvider(Vec<Arc<dyn ContextProvider>>)` that applies providers in order. Needed because `SessionBuilder.context_provider` holds a single `Arc`, and skills must compose with a caller-supplied provider rather than replace it. Chain order is `[skills, caller's provider]` so `PolicyDrivenCompactionProvider` sizes against the prompt that actually ships.
- **[session_builder.rs](crates/harness-engine/src/session_builder.rs)** — add `SessionBuilder::skills(SkillsConfig)`. In `start()`, discover the catalog and register `skill.load`/`skill.read` into `tool_registry` at the same point MCP tools are registered ([session_builder.rs:361](crates/harness-engine/src/session_builder.rs:361)) — reuse that block's `mcp_descriptors` pattern verbatim, including the `root_toolset` merge at [:398](crates/harness-engine/src/session_builder.rs:398), so skill tools land in the toolset whether or not the caller passed an explicit one. Then wrap `self.context_provider` in the chain.
- **[mcp.rs](crates/harness-protocol/src/mcp.rs)** — add a sibling `SkillsSpec { roots: Vec<PathBuf>, include_user_dir: bool }`, and `#[serde(default)] skills: Option<SkillsSpec>` on `RpcRequestBody::CreateSession`. Additive and `serde(default)`, so no `PROTOCOL_VERSION` bump — same treatment `mcp_servers` got.
- **[handler.rs:429](apps/harnessd/src/handler.rs:429)** — convert the spec, mirroring `mcp_config_from_spec`.
- **CLI** — `--skills-dir <path>` (repeatable) on `apps/harness` and `apps/harnessctl`, alongside the existing `--mcp-server` parsing at [main.rs:47](apps/harness/src/main.rs:47).

---

## Workstream 2 — MCP client over HTTP

All inside `crates/tools/mcp`, plus one protocol field.

### Split transport out of `McpClient`

[client.rs](crates/tools/mcp/src/client.rs) currently owns the child process, the pending-request map, and the reader loop in one struct. Introduce:

```rust
#[async_trait]
pub(crate) trait McpTransport: Send + Sync {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError>;
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError>;
    async fn shutdown(&self);
}
```

- `transport/stdio.rs` — the current implementation moved wholesale (spawn, `read_loop`, `handle_line`, `log_stderr`, the `PendingMap`). No behavior change.
- `transport/http.rs` — streamable HTTP: POST JSON-RPC to a single endpoint; handle both `application/json` (one reply) and `text/event-stream` (SSE frames) responses; capture `Mcp-Session-Id` from the `initialize` reply and echo it on every subsequent request; send `MCP-Protocol-Version`. Correlation is per-response, so no pending map is needed. Use `reqwest` with `rustls-tls` + `stream`, matching [harness-tool-web's manifest](crates/tools/web/Cargo.toml).

`McpClient` keeps `initialize` / `list_tools` / `call_tool` unchanged and delegates through the trait. `McpToolExecutor` is untouched.

### Config shape

```rust
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransportConfig,
    pub request_timeout: Option<Duration>,
}

pub enum McpTransportConfig {
    Stdio { command: String, args: Vec<String>, env: HashMap<String,String>, cwd: Option<String> },
    Http  { url: String, headers: HashMap<String,String> },
}
```

`McpServerConfig::new(name, command)` keeps its exact current signature and builds the `Stdio` variant, so [main.rs:66](apps/harness/src/main.rs:66) and the SDK re-export are source-compatible. Add `McpServerConfig::http(name, url)` and `.header(k, v)`. The `.arg/.args/.env/.cwd` builders return `self` unchanged on an `Http` config and are documented as such. The struct-literal construction in [handler.rs:430](apps/harnessd/src/handler.rs:430) needs updating.

### Wire format

Add `#[serde(default)] transport: Option<McpTransportSpec>` to `McpServerSpec` and make `command` `#[serde(default)]`. `None` means "the flat `command`/`args`/`env`/`cwd` fields describe a stdio server" — the existing v2 shape. This stays additive, so no `PROTOCOL_VERSION` bump and no `schema/protocol-v3.schema.json`; edit `schema/protocol-v2.schema.json` in place, as `8b35a34` did for `mcp_servers`.

*(Alternative if you'd rather have one clean representation: make `transport` a required tagged enum, bump `PROTOCOL_VERSION` to 3, add `schema/protocol-v3.schema.json`, and update both SDKs' handshake. Cleaner type, meaningfully more work, and only worth it while there are still no external clients.)*

CLI: extend `--mcp-server` parsing to accept `name=http://...` / `name=https://...` as the HTTP form.

---

## Workstream 3 — MCP server mode

### New crate `crates/transports/mcp` (`harness-transport-mcp`)

The key design decision: **build it on `Arc<dyn RpcHandler>`, exactly like the other three transports** ([rpc.rs:25](crates/harness-runtime/src/rpc.rs:25)), not on `harness-engine::Harness`. That keeps the transport layer uniform, reuses the typed RPC contract the assessment calls out as a strength, and means `harnessd` gets MCP server mode by adding one flag. `harness-transport-mcp` then depends only on `harness-protocol` + `harness-runtime`, matching [harness-transport-stdio's manifest](crates/transports/stdio/Cargo.toml).

Structure mirrors `harness-transport-stdio`: a `serve(handler, config, shutdown)` over real stdin/stdout, and a `serve_io(reader, writer, ...)` split out so tests drive it over `tokio::io::duplex()` instead of spawning a process.

### MCP surface exposed

Server-side JSON-RPC: `initialize` → `notifications/initialized` → `tools/list` / `tools/call`, plus `resources/list` / `resources/read`. Reuse the message shapes already defined in [protocol.rs](crates/tools/mcp/src/protocol.rs) where they're symmetric; move the shared JSON-RPC envelope types (`JsonRpcRequest`, `JsonRpcInbound`, error shape) into a small shared module rather than defining them twice.

Tools:

| MCP tool | Maps to |
|:---|:---|
| `harness_create_session` | `RpcRequestBody::CreateSession` |
| `harness_prompt` | `Mutate { Prompt }`, then drain events to completion |
| `harness_cancel` | `Mutate { Cancel }` |
| `harness_list_sessions` | `RpcRequestBody::ListSessions` |

Resources: `harness://session/{id}` → `RpcRequestBody::Snapshot`, enumerated via `ListSessions`.

**`harness_prompt` is the one with real substance.** MCP `tools/call` is request/response, but the harness is event-streamed. It must `handler.subscribe(session_id)` *before* sending the mutation (otherwise early events are lost to the broadcast channel), then drain until `AgentEvent::Completed` ([events.rs:216](crates/harness-protocol/src/events.rs:216)), accumulating assistant text and a tool-call summary into the `CallToolResult`. Honor the caller's cancellation and a configurable ceiling; on `RecvError::Lagged`, fall back to `Snapshot` rather than returning a truncated transcript.

An MCP client cannot know about integrations, so `CreateSession`'s `integration` / `integration_config` come from server-mode config, not the tool call.

### `harnessd` wiring

Add `--mcp-stdio` alongside `--unix-socket` / `--tcp` / `--stdio` in [main.rs](apps/harnessd/src/main.rs), plus `--mcp-default-integration <id>` and `--mcp-workspace-root <path>` to supply what MCP clients can't. `--mcp-stdio` and `--stdio` are mutually exclusive — both claim stdout exclusively.

Also add `mcp_server_mode: bool` to `ProtocolCapabilities` ([rpc.rs:243](crates/harness-protocol/src/rpc.rs:243)) so clients can detect it over the handshake.

---

## Cross-cutting

- **[xtask/src/main.rs:165](xtask/src/main.rs:165)** — `forbidden_reason` already bans `harness-tool-*` and `harness-transport-*` from core, so `harness-tool-skills` and `harness-transport-mcp` are covered by naming alone. Add an explicit `harness-skills` arm so the non-prefixed crate can't leak into core either.
- **`deny.toml`** — no new advisory exceptions expected; the flat-frontmatter parser exists specifically to avoid one.
- **CI** — the existing matrix ([ci.yml](.github/workflows/ci.yml)) covers everything: `cargo clippy --workspace --all-targets --all-features -- -D warnings` is the gate that will catch the most here.
- **README** — the MCP section needs the server-mode and HTTP additions; skills need a new section with a worked `SKILL.md` example.
- **SDKs** — `sdk/typescript/src/types.ts` and `sdk/rust/src/lib.rs` need the new optional `CreateSession` fields.

---

## Verification

**Skills**
1. `cargo test -p harness-skills -p harness-tool-skills` — unit tests over `tempfile` dirs: frontmatter parse (valid, missing required field, no fence), workspace-overrides-user precedence, and `skill.read` rejecting `../` traversal and escaping symlinks.
2. End-to-end: create `.rusty/skills/pdf-report/SKILL.md` in a scratch dir, run `cargo run -p harness -- --skills-dir <dir>`, prompt something the skill covers, and confirm from the transcript that the catalog line is in the system prompt and the model called `skill.load` before acting.
3. Assert the negative too: the skill *body* must not appear in `ExecutionRequest::system_prompt` on the first turn.

**MCP HTTP**
4. `cargo test -p harness-tool-mcp` — the existing [client_e2e.rs](crates/tools/mcp/tests/client_e2e.rs) against `fake_mcp_server.rs` must pass unchanged, proving the stdio refactor is behavior-preserving.
5. New HTTP test with a `tokio`-hosted stub server covering both the `application/json` and `text/event-stream` response paths, `Mcp-Session-Id` capture and echo, and timeout.
6. Live check against a real hosted MCP server via `--mcp-server name=https://...`.

**MCP server mode**
7. `cargo test -p harness-transport-mcp` — `serve_io` over `tokio::io::duplex()` with a fake `RpcHandler`: handshake, `tools/list`, and a `harness_prompt` that returns only after a `Completed` event. Include a test that events emitted immediately after the mutation are still captured, pinning the subscribe-before-send ordering.
8. `npx @modelcontextprotocol/inspector cargo run -p harnessd -- --mcp-stdio --mcp-default-integration anthropic-api` — confirm the tool list and a real prompt round-trip.
9. Point Claude Desktop's `mcpServers` config at the `harnessd --mcp-stdio` command and drive a session from it. This is the actual acceptance criterion for the P0 item.

**Whole workspace**
10. `cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, and `cargo run --manifest-path xtask/Cargo.toml -- check-deps`.

# MCP server mode, MCP HTTP transport, and filesystem skills

Status doc for the three gaps the comparative assessment flagged as blocking
open-source launch. Two are now closed; one remains.

| Phase | Scope | Status |
|:--|:--|:--|
| 1 | Filesystem skills (`SKILL.md`), runtime-extensible | **Done** |
| 2 | MCP client over HTTP, alongside stdio | **Done** |
| 3 | MCP server mode — expose the engine *as* an MCP server | **Not started** |

Verification at the time of writing: `cargo test --workspace --all-targets`
passes 688 tests across 62 suites, with `cargo fmt --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
`xtask check-deps` all clean.

---

## Context

The assessment identified four gaps. One of them turned out to already be
closed, which is worth recording so it isn't re-investigated:

- **MCP client — was already done.** `crates/tools/mcp` shipped a complete
  stdio client in `8b35a34`, wired through `SessionBuilder::mcp_server` →
  `McpToolExecutor` → `SimpleToolRegistry`, with `McpServerSpec` on the
  `CreateSession` RPC. The assessment's "MCP client only, stdio-only" line
  described what had already landed.
- **Skills — genuinely absent.** Zero occurrences of the word anywhere in
  the tree. The compile-time-only `Plugin` trait in
  `harness-extension-api` was the whole extension story.
- **MCP HTTP transport — absent.** `McpClient` hard-coded
  `ChildStdin`/`ChildStdout`.
- **MCP server mode — absent.** The engine could consume MCP servers but not
  present itself as one, so IDEs needed a custom SDK.

---

## Architectural constraints

The verified internal dependency DAG:

```
harness-protocol ─┬─ harness-core ── harness-runtime ─┬─ harness-context ── harness-skills
                  ├─ harness-model                    ├─ harness-transport-{ipc,stdio,websocket}
                  └─ harness-session-store            └─ harness-engine
harness-workspace ─┐
harness-tools ─────┴─ harness-tool-{filesystem,shell,git,web,mcp,skills}
```

Three rules the work had to respect:

- **`xtask check-deps` guards `harness-core` and `harness-protocol` only.**
  `forbidden_reason` bans, transitively from those two roots: tokio's
  `net`/`process`/`signal`/`fs`/`io-std` features, any `harness-integration-*`
  / `harness-tool-*` / `harness-transport-*` crate, and a name list that
  already included `rmcp`. `harness-tool-skills` is covered by prefix;
  **`harness-skills` is not**, so it needed an explicit arm — added.
- **`harness-protocol` stays I/O-free.** Wire types mirror the real config
  types rather than reusing them (`McpServerSpec`, `SkillsSpec`).
- **Cross-cutting behavior attaches as a backend decorator**, following
  `ToolAdvertisingBackend` and `ContextAssemblingBackend`. Nothing in this
  work touched `harness-core`.

---

## Phase 1 — Filesystem skills (done)

A skill is a directory with a `SKILL.md` (YAML frontmatter + markdown body)
plus any files it references. Dropping one into `.harness/skills/` adds a
capability with no recompilation.

**`crates/harness-skills`** — discovery, parsing, catalog, context provider.
Depends only on `harness-context`, `harness-protocol`, `harness-workspace`
(not `harness-runtime`: `ContextProvider::assemble` takes
`harness_runtime::traits::Workspace`, which is a re-export of
`harness_workspace::Workspace` — the same type from the lighter crate).

**`crates/tools/skills`** — `skill.load` (full instructions + bundled-file
listing) and `skill.read` (one bundled file, scoped to that skill's dir).
Separate from `fs.read` because skill dirs sit outside the workspace root,
where `FsWorkspace`'s traversal guard correctly refuses.

### Design decisions worth keeping

- **Progressive disclosure is the point.** Only `name: description` reaches
  the system prompt; bodies stay on disk until the model calls `skill.load`.
  Thirty skills cost thirty lines per request, not thirty documents.
  `crates/harness-engine/tests/skills_e2e.rs` asserts the *negative* — that
  instruction bodies never appear in the assembled `ExecutionRequest`.
- **Hand-rolled flat-YAML frontmatter parser** rather than a YAML crate.
  `serde_yaml` is archived, and `deny.toml` sets `yanked = "deny"` while
  carrying exactly one grudging advisory exception. Same reasoning that
  produced a hand-rolled MCP JSON-RPC client instead of `rmcp`.
- **`.harness/skills`, not `.rusty/skills`.** `.harness/` is the only
  per-project convention the code actually establishes (see harnessd's
  `--sessions-dir` default). The stale `.rusty/` directory in the repo is
  from another tool.
- **Discovery errors are never fatal.** A malformed `SKILL.md` logs at
  `warn` and is skipped; its siblings still load. Failing session start over
  a stale `--skills-dir` would be worse than a warning.
- **Precedence, later wins:** `$HOME/.harness/skills` → `<workspace>/.harness/skills`
  → explicit `--skills-dir` roots.
- **`skill.read` scoping is two checks, both load-bearing:** reject absolute
  paths and `..` components up front, then canonicalize both root and target
  and require the prefix. The second closes the *symlink* escape, which a
  component check alone misses. Pinned by a dedicated test.
- **Defaults differ by surface.** On in the standalone TUI (local,
  single-user, the operator's own directories); off over RPC, where the
  daemon's `$HOME` is not the caller's.

`ChainedContextProvider` already existed and was reused, not rebuilt.

---

## Phase 2 — MCP client over HTTP (done)

`McpClient` now sits on an internal `McpTransport` trait with two
implementations.

**The seam is cut at whole JSON-RPC calls (`request`/`notify`), not at raw
bytes**, because the transports correlate replies differently: stdio
multiplexes everything over one pipe and needs an id counter plus a
pending-request map; streamable HTTP gets its reply on the POST's own
response and would carry that machinery as dead weight.

- `transport/stdio.rs` — the previous implementation moved verbatim. The
  regression proof is that `tests/client_e2e.rs` passes **unmodified**.
- `transport/http.rs` — POST JSON-RPC; handles both `application/json` and
  `text/event-stream` replies. SSE is consumed **incrementally** and returns
  the moment the matching reply arrives, because a server may keep the
  stream open after answering and reading to EOF would stall every call.
  Skips server-initiated notifications that precede the real reply.
  Captures `Mcp-Session-Id` at `initialize` and echoes it thereafter.
  Typed failures: `UnexpectedContentType` (an HTML login page is not a JSON
  parse error), `HttpStatus`, `Timeout`, `InvalidUrl`.

**Wire compatibility without a version bump.** `McpServerSpec` gained an
optional tagged `transport` member; its absence means the flat
`command`/`args`/`env`/`cwd` fields describe a stdio server — the original
shape. All reads go through `McpServerSpec::resolve_transport()` so the
legacy form is handled in exactly one place. `PROTOCOL_VERSION` stays 2.

CLI: `--mcp-server name=command[,args...]` for stdio,
`name=https://host/mcp` for HTTP. A URL is never a plausible executable
name, so no extra flag is needed to disambiguate.

---

## Phase 3 — MCP server mode (not started)

Expose the engine *as* an MCP server so Claude Desktop / Cursor / VS Code can
drive it without a custom SDK. This is the P0 item.

### Approach

New crate `crates/transports/mcp` (`harness-transport-mcp`), built on
**`Arc<dyn RpcHandler>`** exactly like the other three transports — not on
`harness_engine::Harness`. That keeps the dependency set to
`harness-protocol` + `harness-runtime`, gets `harnessd` server mode with one
flag, and routes MCP-created sessions through the same admission cache and
revision tracking as every other client.

Structure mirrors `harness-transport-stdio`: `serve()` over real
stdin/stdout, `serve_io()` split out so tests drive it over
`tokio::io::duplex()`, and a dedicated writer task so response lines never
interleave.

### Surface

`initialize` → `notifications/initialized` → `tools/list`, `tools/call`,
`resources/list`, `resources/read`.

| MCP tool | Maps to |
|:--|:--|
| `harness_create_session` | `CreateSession` (integration/workspace/toolset from server config) |
| `harness_prompt` | `Mutate { Prompt }` + drain to completion |
| `harness_cancel` | `Mutate { Cancel }` |
| `harness_list_sessions` | `ListSessions` |

Resources: `harness://session/{id}` renders a transcript from
`handler.events_since(id, 0)`. **No protocol addition is needed** —
`SessionSnapshotWire` carries only status/usage, but `events_since` already
returns every durable event.

### `harness_prompt` — the part with real substance

MCP `tools/call` is request/response; the harness is event-streamed.

1. `handler.subscribe(session_id)` **before** sending the mutation — a
   `broadcast::Receiver` only delivers messages sent after it exists, so
   subscribing afterwards loses the opening events.
2. Send `Mutate { Prompt }`. If the reply isn't `Admission { Accepted | AcceptedApplied }`,
   return an error rather than waiting on a run that never started.
3. Drain, accumulating `AssistantTextDelta.delta` (the only source of
   assistant text) and `ToolCallCompleted.result.output_preview`. Terminate
   on `AgentEvent::Completed`, mapping `Cancelled`/`Failed` to
   `is_error: true`. Also terminate on `AgentEvent::Failed`.
4. **`PermissionRequested` is a deadlock hazard** — a toolset with
   `PermissionMode::Ask` parks the run forever with no MCP-side way to
   answer. Default to an all-`Allow` toolset and return `is_error` rather
   than hanging.
5. On `RecvError::Lagged`, fall back to `events_since` rather than returning
   silently truncated text.
6. Bound by a configurable timeout; honor the transport's cancellation token.

### Not doing: a shared `harness-mcp-wire` crate

An earlier draft proposed extracting the JSON-RPC envelope types so client
and server don't define them twice. Rejected after reading both sides: the
client's types are serialize-only and borrowed (`JsonRpcRequest<'a>`), while
the server needs the mirror images — deserialize incoming requests,
serialize outgoing responses. Sharing would mean making everything
bidirectional and owned, which is more churn than the ~40 lines it saves.

The one value that must not drift is `MCP_PROTOCOL_VERSION`, since both
sides negotiate it. Hoist that single const into `harness_protocol::mcp` —
a pure protocol constant in the crate that already owns MCP wire types — and
let each side keep its own serde-direction-appropriate structs.

### `harnessd` wiring

`--mcp-stdio`, `--mcp-integration <id>`, `--mcp-integration-config <json>`,
`--mcp-workspace-root <path>`. `--mcp-stdio` and `--stdio` must be mutually
exclusive; both claim stdout.

Advertising `mcp_server_mode` on `ProtocolCapabilities` requires a small
refactor first: all three transports construct `ProtocolCapabilities::default()`
at dispatch time rather than being told what the host enabled, so a truthful
value means threading capabilities into `serve()`. Setting the default to
`true` unconditionally would be a lie.

---

## Build performance note

Unrelated to the features, but it dominated iteration time and is worth
recording. `target/` had grown to **72 GB / 969k files**, with 605k in
`debug/deps` alone — cargo never garbage-collects that directory. Test runs
took >10 minutes of which **5.78 s** was actually running tests and 1.12 s
was compiling; the rest was filesystem overhead resolving artifacts and
launching ~50 test binaries.

Fixed by adding profile tuning to the root `Cargo.toml` (`debug =
"line-tables-only"` for workspace crates, `debug = false` for the ~390
dependencies via `[profile.dev.package."*"]`) and deleting `target/debug`.

| | Before | After |
|:--|:--|:--|
| `target/` size | 72 GB | 4.6 GB |
| Warm `cargo test --workspace` | >600 s | **6 s** |
| Cold full rebuild + all tests | — | **64 s** |

It will regrow, mainly because `cargo clippy --all-features` and `cargo test`
resolve features differently and each keeps a complete separate artifact set.
`cargo sweep --time 30` prunes by age without a full rebuild.

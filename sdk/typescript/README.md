# @rusty/harness-sdk

TypeScript client for harnessd protocol v2.

The SDK performs the required hello handshake and exposes:

- session create, list, strict restore, snapshot, and close;
- prompt, steer, follow-up, cancel, and permission mutations;
- client command IDs, optional expected revisions, and typed admission receipts;
- structured RPC errors with stable codes, categories, retryability, and correlation fields;
- resumable event subscription and explicit event-gap callbacks;
- exact Rust wire shapes for toolsets, MCP server specs, and attachment byte arrays.

`HarnessClient.capabilities.durable_idempotency` is currently false. Reusing
a command ID protects ambiguous retries while the daemon remains alive, but
admission history is not yet restored after a daemon restart.

The Rust DTOs are authoritative. The language-neutral mirror is
[`protocol-v2.schema.json`](../../schema/protocol-v2.schema.json).

## Quick start

The only transport shipped today is a managed stdio sidecar: the SDK spawns
`harnessd --stdio` as a child process and owns its lifecycle (see
[`StdioSidecarTransport`](src/transport.ts) — WebSocket/Unix/Windows IPC
transports are planned, not yet implemented, per `sdk_plan.md` SDK-301/303).

```ts
import { HarnessClient, StdioSidecarTransport } from "@rusty/harness-sdk";

const transport = new StdioSidecarTransport({
  command: "/path/to/harnessd", // build with `cargo build --release -p harnessd`
});
const client = await HarnessClient.connect(transport);

const session = await client.createSession({
  workspaceRoot: "/path/to/workspace",
  integration: "anthropic", // reads ANTHROPIC_API_KEY
});

for await (const envelope of session.events()) {
  if ("AssistantTextDelta" in envelope.event) {
    process.stdout.write(envelope.event.AssistantTextDelta.delta);
  }
  if ("Completed" in envelope.event) break;
}

await session.prompt("In one short sentence, what is a Rust trait?");
```

Call `session.events(sinceSeq)` again after a reconnect to resume without
gaps or duplicates — pass the highest `session_sequence` you saw. See the
main [README's durability section](../../README.md#durability-and-resume)
for what's replayable.

## MCP servers

Connect [MCP](https://modelcontextprotocol.io) servers over stdio by passing
`mcpServers` to `createSession` — the wire-shaped mirror of Rust's
`McpServerSpec` (see `crates/harness-protocol/src/mcp.rs` and the main
[README's MCP servers section](../../README.md#mcp-servers)):

```ts
const session = await client.createSession({
  workspaceRoot: "/path/to/workspace",
  integration: "anthropic",
  mcpServers: [
    {
      name: "filesystem",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
    },
  ],
});
```

Every tool the server advertises is discovered at session start and
registered as `mcp.filesystem.<tool-name>`, indistinguishable from a
built-in tool to the model. `mcpServers` is entirely optional and omitted
from the wire request (not sent as `[]`) when you don't pass it.

## Development

```sh
npm ci
npm run build
npm test
```

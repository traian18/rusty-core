import assert from "node:assert/strict";
import test from "node:test";

import { HarnessClient } from "../client.js";
import type { RpcRequest, RpcResponse, UserInput } from "../types.js";
import type { Transport } from "../transport.js";

const SESSION_ID = "00000000-0000-0000-0000-000000000001";

class MockTransport implements Transport {
  readonly requests: RpcRequest[] = [];
  private messageListener: ((response: RpcResponse) => void) | undefined;
  private closeListener: ((reason?: Error) => void) | undefined;

  send(request: RpcRequest): void {
    this.requests.push(request);
    const body = request.body;
    if (body.type === "hello") {
      this.reply(request, {
        type: "hello",
        payload: {
          protocol_version: 2,
          capabilities: {
            resumable_subscribe: true,
            lifecycle_commands: true,
            typed_errors: true,
            mutation_admission: true,
            session_restore: true,
            event_gap_signals: true,
            durable_idempotency: false,
            pause_resume: false,
          },
        },
      });
    } else if (body.type === "create_session") {
      this.reply(request, {
        type: "session_created",
        payload: { session_id: SESSION_ID },
      });
    } else if (body.type === "mutate") {
      this.reply(request, {
        type: "admission",
        payload: {
          metadata: body.payload.metadata,
          result: { type: "accepted" },
          session_revision: 1,
        },
      });
    } else if (body.type === "list_sessions") {
      this.reply(request, {
        type: "sessions_listed",
        payload: { sessions: [] },
      });
    }
  }

  private reply(request: RpcRequest, body: RpcResponse["body"]): void {
    this.messageListener?.({ id: request.id, body });
  }

  onMessage(listener: (response: RpcResponse) => void): void {
    this.messageListener = listener;
  }

  onClose(listener: (reason?: Error) => void): void {
    this.closeListener = listener;
  }

  async close(): Promise<void> {
    this.closeListener?.();
  }
}

test("protocol v2 creates sessions with the Rust AgentToolset shape", async () => {
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  await client.createSession({ workspaceRoot: "/workspace", integration: "test" });

  assert.deepEqual(transport.requests[1]?.body, {
    type: "create_session",
    payload: {
      workspace_root: "/workspace",
      integration: "test",
      integration_config: {},
      toolset: { tools: {} },
    },
  });
});

test("createSession omits mcp_servers entirely when none are configured", async () => {
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  await client.createSession({ workspaceRoot: "/workspace", integration: "test" });

  const payload = transport.requests[1]?.body;
  assert.equal(payload?.type, "create_session");
  assert.ok(
    payload && payload.type === "create_session" && !("mcp_servers" in payload.payload),
    "mcp_servers must be omitted, not sent as [], when unset",
  );
});

test("createSession forwards MCP server specs verbatim in wire shape", async () => {
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  await client.createSession({
    workspaceRoot: "/workspace",
    integration: "test",
    mcpServers: [
      { name: "filesystem", command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
    ],
  });

  const payload = transport.requests[1]?.body;
  assert.equal(payload?.type, "create_session");
  assert.deepEqual(
    payload?.type === "create_session" ? payload.payload.mcp_servers : undefined,
    [
      { name: "filesystem", command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
    ],
  );
});

test("mutations carry command identity and return admission revisions", async () => {
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  const session = await client.createSession({
    workspaceRoot: "/workspace",
    integration: "test",
  });
  const receipt = await session.prompt("hello", {
    commandId: "00000000-0000-0000-0000-000000000002",
  });

  assert.equal(receipt.result.type, "accepted");
  assert.equal(receipt.sessionRevision, 1);
  const mutation = transport.requests[2]?.body;
  assert.equal(mutation?.type, "mutate");
  if (mutation?.type === "mutate") {
    assert.equal(mutation.payload.metadata.session_id, SESSION_ID);
    assert.equal(mutation.payload.metadata.expected_session_revision, 0);
    assert.equal(mutation.payload.command.type, "prompt");
  }
});

test("listSessions uses the v2 lifecycle operation", async () => {
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  assert.deepEqual(await client.listSessions(), []);
});

test("attachment bytes use serde Vec<u8> JSON arrays", () => {
  const input = {
    text: "inspect",
    attachments: [{ mime_type: "text/plain", data: [104, 105] }],
  } satisfies UserInput;
  assert.deepEqual(JSON.parse(JSON.stringify(input)), input);
});

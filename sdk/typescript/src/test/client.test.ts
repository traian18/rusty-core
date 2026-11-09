import assert from "node:assert/strict";
import test from "node:test";

import { HarnessClient } from "../client.js";
import type { AgentEvent, AgentEventEnvelope, CompactionOutcome, ContextUnavailableReason, RpcRequest, RpcResponse, UserInput } from "../types.js";
import type { Transport } from "../transport.js";

const SESSION_ID = "00000000-0000-0000-0000-000000000001";

class MockTransport implements Transport {
  executionPolicy = true;
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
            execution_policy: this.executionPolicy,
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

function visibilityEnvelope(eventId: string, sequence: number, event: AgentEvent): AgentEventEnvelope {
  return {
    event_id: eventId, session_id: SESSION_ID, agent_id: "agent-1", parent_agent_id: null,
    run_id: "run-1", agent_sequence: sequence, session_sequence: sequence,
    timestamp: "2025-01-01T00:00:00Z", visibility: "User", event,
  } satisfies AgentEventEnvelope;
}

test("context visibility wire fixtures preserve request and update order", () => {
  const reasons: ContextUnavailableReason[] = ["unknown_model_window", "unusable_input_budget", "model_window_mismatch"];
  const started = visibilityEnvelope("start", 1, { ContextRequestStarted: { request_id: "request-1", request_start_order: 1, model: "model-1" } });
  const available = visibilityEnvelope("available", 3, { ContextUpdated: {
    request_id: "request-2", request_start_order: 2, model: null,
    availability: { available: { estimated_input_tokens: 10, input_budget: 20, model_window: 30, remaining_input_tokens: 10, usage_percent: 65535 } },
    compaction_started: false, compaction_ended: false,
  } });
  const unavailable = reasons.map((reason, index) => visibilityEnvelope(`unavailable-${index}`, 4 + index, { ContextUpdated: {
    request_id: "request-2", request_start_order: 2, model: null, availability: { unavailable: { reason } },
    compaction_started: false, compaction_ended: false,
  } }));
  const fixtures = [available, started, ...unavailable];
  const wire = JSON.parse(JSON.stringify(fixtures));
  assert.deepEqual(fixtures, wire);
  assert.deepEqual(wire[0].event, { ContextUpdated: {
    request_id: "request-2", request_start_order: 2, model: null,
    availability: { available: { estimated_input_tokens: 10, input_budget: 20, model_window: 30, remaining_input_tokens: 10, usage_percent: 65535 } },
    compaction_started: false, compaction_ended: false,
  } });
  assert.deepEqual(wire[1].event, { ContextRequestStarted: { request_id: "request-1", request_start_order: 1, model: "model-1" } });
  assert.deepEqual(wire.slice(2).map((item: AgentEventEnvelope) => (item.event as { ContextUpdated: { availability: unknown } }).ContextUpdated.availability), reasons.map((reason) => ({ unavailable: { reason } })));
  assert.deepEqual(wire.map((item: AgentEventEnvelope) => item.agent_sequence), [3, 1, 4, 5, 6]);
  assert.equal(wire[0].event.ContextUpdated.request_start_order, 2);
  assert.equal(wire[1].event.ContextRequestStarted.request_start_order, 1);
  assert.equal(wire[0].session_id, SESSION_ID);
  assert.equal(wire[0].agent_id, "agent-1");
  assert.equal(wire[0].run_id, "run-1");
});

test("compaction visibility wire fixtures correlate overlapping operations and redelivery", () => {
  const request = { request_id: "request-1", request_start_order: 1 };
  const standard = { ...request, operation_id: "operation-standard", compaction_type: "standard" as const };
  const background = { ...request, operation_id: "operation-background", compaction_type: "background_summary" as const };
  const fixtures = [
    visibilityEnvelope("event-1", 1, { CompactionStarted: { ...standard, before_estimated_input_tokens: 20, compaction_started: true, compaction_ended: false } }),
    visibilityEnvelope("event-2", 2, { CompactionStarted: { ...background, compaction_started: true, compaction_ended: false } }),
    visibilityEnvelope("event-3", 3, { CompactionFinished: { ...background, outcome: "success", compaction_started: false, compaction_ended: true } }),
    visibilityEnvelope("event-4", 4, { CompactionFinished: { ...standard, outcome: "success", before_estimated_input_tokens: 20, after_estimated_input_tokens: 10, compaction_started: false, compaction_ended: true } }),
  ];
  const serialized = JSON.stringify(fixtures);
  const wire = JSON.parse(serialized);
  assert.deepEqual(fixtures, wire);
  assert.deepEqual(wire.map((item: AgentEventEnvelope) => [item.event_id, item.session_id, item.agent_id, item.run_id, item.agent_sequence, item.session_sequence]), [
    ["event-1", SESSION_ID, "agent-1", "run-1", 1, 1], ["event-2", SESSION_ID, "agent-1", "run-1", 2, 2],
    ["event-3", SESSION_ID, "agent-1", "run-1", 3, 3], ["event-4", SESSION_ID, "agent-1", "run-1", 4, 4],
  ]);
  assert.deepEqual(wire.map((item: AgentEventEnvelope) => Object.keys(item.event)[0]), ["CompactionStarted", "CompactionStarted", "CompactionFinished", "CompactionFinished"]);
  assert.equal(wire[0].event.CompactionStarted.operation_id, wire[3].event.CompactionFinished.operation_id);
  assert.equal(wire[1].event.CompactionStarted.operation_id, wire[2].event.CompactionFinished.operation_id);
  assert.deepEqual(wire[1].event.CompactionStarted, { ...background, compaction_started: true, compaction_ended: false });
  assert.deepEqual(wire[2].event.CompactionFinished, { ...background, outcome: "success", compaction_started: false, compaction_ended: true });
  assert.deepEqual(wire[3].event.CompactionFinished, { ...standard, outcome: "success", before_estimated_input_tokens: 20, after_estimated_input_tokens: 10, compaction_started: false, compaction_ended: true });
  const unsuccessful: CompactionOutcome[] = ["failure", "cancelled"];
  for (const outcome of unsuccessful) {
    const fixture = visibilityEnvelope(outcome, 5, { CompactionFinished: { ...standard, outcome, compaction_started: false, compaction_ended: true } });
    const payload = JSON.parse(JSON.stringify(fixture)).event.CompactionFinished;
    assert.deepEqual(payload, { ...standard, outcome, compaction_started: false, compaction_ended: true });
    assert.equal("after_estimated_input_tokens" in payload, false);
  }
  const redelivered = JSON.parse(serialized)[2];
  assert.deepEqual(redelivered, wire[2]);
  assert.deepEqual([redelivered.event_id, redelivered.session_id, redelivered.agent_id, redelivered.run_id, redelivered.agent_sequence, redelivered.event.CompactionFinished.operation_id], ["event-3", SESSION_ID, "agent-1", "run-1", 3, "operation-background"]);
});

test("createSession forwards skill permissions and refuses an older harness", async () => {
  const executionPolicy = { mode: "plan" as const, enabled_tools: ["write_file"], allowed_mcp_servers: [] };
  const transport = new MockTransport();
  const client = await HarnessClient.connect(transport);
  await client.createSession({ workspaceRoot: "/workspace", integration: "test", executionPolicy });
  const body = transport.requests[1]?.body;
  assert.equal(body?.type, "create_session");
  assert.deepEqual(body?.type === "create_session" && body.payload.execution_policy, executionPolicy);
  const oldTransport = new MockTransport();
  oldTransport.executionPolicy = false;
  const oldClient = await HarnessClient.connect(oldTransport);
  await assert.rejects(oldClient.createSession({ workspaceRoot: "/workspace", integration: "test", executionPolicy }), /refusing to discard/);
  assert.equal(oldTransport.requests.length, 1);
});

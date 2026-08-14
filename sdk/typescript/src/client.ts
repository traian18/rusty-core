/**
 * High-level protocol-v2 client over one connected harnessd transport.
 */

import { randomUUID } from "node:crypto";

import {
  HarnessRpcError,
  HarnessTimeoutError,
  HarnessTransportClosedError,
  HarnessVersionMismatchError,
} from "./errors.js";
import { HarnessSession } from "./session.js";
import type { Transport } from "./transport.js";
import type {
  AdmissionResult,
  AgentEventEnvelope,
  AgentToolset,
  McpServerSpec,
  SkillsSpec,
  MutationCommand,
  MutationMetadata,
  PermissionDecision,
  PermissionId,
  ProtocolCapabilities,
  RpcRequestBody,
  RpcResponse,
  RpcResponseBody,
  SessionId,
  SessionSnapshotWire,
  SessionSummaryWire,
} from "./types.js";
import { PROTOCOL_VERSION } from "./types.js";

export interface ConnectOptions {
  requestTimeoutMs?: number;
}

export interface CreateSessionOptions {
  workspaceRoot: string;
  integration: string;
  integrationConfig?: unknown;
  toolset?: AgentToolset;
  /**
   * MCP servers to connect over stdio at session start; discovered tools
   * merge into `toolset` under `mcp.<name>.<tool>` ids. Omitted entirely
   * (rather than sent as `[]`) when unset, matching the Rust side's
   * `#[serde(default)]` on `RpcRequestBody::CreateSession.mcp_servers`.
   */
  mcpServers?: McpServerSpec[];
  /**
   * Directories to scan for `SKILL.md` files at session start. Discovered
   * skills add a one-line description each to the system prompt and merge
   * `skill.load`/`skill.read` into `toolset`. Omitted entirely when unset,
   * which disables skills — matching the Rust side's `Option<SkillsSpec>`.
   */
  skills?: SkillsSpec;
}

export interface RestoreSessionOptions {
  sessionId: SessionId;
  workspaceRoot: string;
  toolset?: AgentToolset;
}

export interface MutationOptions {
  commandId?: string;
  expectedSessionRevision?: number | null;
  runId?: string | null;
  traceId?: string | null;
}

export interface AdmissionReceipt {
  metadata: MutationMetadata;
  result: AdmissionResult;
  sessionRevision: number;
}

export interface EventGap {
  sessionId: SessionId;
  lastDeliveredSequence: number;
  dropped: number;
  cursorExpired: boolean;
}

type PendingReply = {
  resolve: (body: RpcResponseBody) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

export class HarnessClient {
  private nextRequestId = 1;
  private readonly pending = new Map<number, PendingReply>();
  private readonly eventListeners = new Map<
    SessionId,
    Set<(event: AgentEventEnvelope) => void>
  >();
  private readonly gapListeners = new Map<SessionId, Set<(gap: EventGap) => void>>();
  private readonly sessionRevisions = new Map<SessionId, number>();
  private closed = false;
  private readonly requestTimeoutMs: number;

  private constructor(
    private readonly transport: Transport,
    public readonly capabilities: ProtocolCapabilities,
    requestTimeoutMs: number,
  ) {
    this.requestTimeoutMs = requestTimeoutMs;
    this.transport.onMessage((response) => this.handleResponse(response));
    this.transport.onClose((reason) => this.handleClose(reason));
  }

  static async connect(
    transport: Transport,
    options: ConnectOptions = {},
  ): Promise<HarnessClient> {
    const bootstrap = new HarnessClient(
      transport,
      {
        resumable_subscribe: false,
        lifecycle_commands: false,
        typed_errors: false,
        mutation_admission: false,
        session_restore: false,
        event_gap_signals: false,
        durable_idempotency: false,
        pause_resume: false,
      },
      options.requestTimeoutMs ?? 30_000,
    );
    const body = await bootstrap.request({
      type: "hello",
      payload: { protocol_version: PROTOCOL_VERSION },
    });
    if (body.type !== "hello") {
      throw new HarnessRpcError("expected a hello response");
    }
    if (body.payload.protocol_version !== PROTOCOL_VERSION) {
      throw new HarnessVersionMismatchError(
        PROTOCOL_VERSION,
        body.payload.protocol_version,
      );
    }
    (bootstrap as { capabilities: ProtocolCapabilities }).capabilities =
      body.payload.capabilities;
    return bootstrap;
  }

  async createSession(options: CreateSessionOptions): Promise<HarnessSession> {
    const body = await this.request({
      type: "create_session",
      payload: {
        workspace_root: options.workspaceRoot,
        integration: options.integration,
        integration_config: options.integrationConfig ?? {},
        toolset: options.toolset ?? { tools: {} },
        ...(options.mcpServers ? { mcp_servers: options.mcpServers } : {}),
        ...(options.skills ? { skills: options.skills } : {}),
      },
    });
    if (body.type !== "session_created") {
      throw new HarnessRpcError("expected session_created response");
    }
    this.sessionRevisions.set(body.payload.session_id, 0);
    return new HarnessSession(this, body.payload.session_id);
  }

  async listSessions(): Promise<SessionSummaryWire[]> {
    const body = await this.request({ type: "list_sessions" });
    if (body.type !== "sessions_listed") {
      throw new HarnessRpcError("expected sessions_listed response");
    }
    return body.payload.sessions;
  }

  async restoreSession(options: RestoreSessionOptions): Promise<HarnessSession> {
    if (!this.capabilities.session_restore) {
      throw new HarnessRpcError("the connected daemon does not support session restore");
    }
    const body = await this.request({
      type: "restore_session",
      payload: {
        session_id: options.sessionId,
        workspace_root: options.workspaceRoot,
        toolset: options.toolset ?? { tools: {} },
      },
    });
    if (body.type !== "session_restored") {
      throw new HarnessRpcError("expected session_restored response");
    }
    this.sessionRevisions.set(body.payload.session_id, body.payload.session_revision);
    return new HarnessSession(this, body.payload.session_id);
  }

  prompt(
    sessionId: SessionId,
    text: string,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    return this.mutate(
      sessionId,
      { type: "prompt", payload: { text, attachments: [] } },
      options,
    );
  }

  steer(
    sessionId: SessionId,
    text: string,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    this.requireLifecycleCommands();
    return this.mutate(
      sessionId,
      { type: "steer", payload: { text, attachments: [] } },
      options,
    );
  }

  followUp(
    sessionId: SessionId,
    text: string,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    this.requireLifecycleCommands();
    return this.mutate(
      sessionId,
      { type: "follow_up", payload: { text, attachments: [] } },
      options,
    );
  }

  cancel(sessionId: SessionId, options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.mutate(sessionId, { type: "cancel" }, options);
  }

  resolvePermission(
    sessionId: SessionId,
    id: PermissionId,
    decision: PermissionDecision,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    return this.mutate(
      sessionId,
      { type: "resolve_permission", payload: { id, decision } },
      options,
    );
  }

  async snapshot(sessionId: SessionId): Promise<SessionSnapshotWire> {
    const body = await this.requestForSession(sessionId, { type: "snapshot" });
    if (body.type !== "snapshot") {
      throw new HarnessRpcError("expected snapshot response");
    }
    return body.payload;
  }

  async subscribe(
    sessionId: SessionId,
    onEvent: (event: AgentEventEnvelope) => void,
    sinceSeq: number | null = null,
    onGap?: (gap: EventGap) => void,
  ): Promise<() => void> {
    let listeners = this.eventListeners.get(sessionId);
    if (!listeners) {
      listeners = new Set();
      this.eventListeners.set(sessionId, listeners);
    }
    listeners.add(onEvent);
    if (onGap) {
      let gaps = this.gapListeners.get(sessionId);
      if (!gaps) {
        gaps = new Set();
        this.gapListeners.set(sessionId, gaps);
      }
      gaps.add(onGap);
    }

    await this.requestForSession(sessionId, {
      type: "subscribe",
      payload: { since_seq: sinceSeq },
    });

    return () => {
      listeners?.delete(onEvent);
      if (onGap) this.gapListeners.get(sessionId)?.delete(onGap);
    };
  }

  async closeSession(
    sessionId: SessionId,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    const receipt = await this.mutate(sessionId, { type: "close_session" }, options);
    this.eventListeners.delete(sessionId);
    this.gapListeners.delete(sessionId);
    this.sessionRevisions.delete(sessionId);
    return receipt;
  }

  async close(): Promise<void> {
    await this.transport.close();
  }

  private async mutate(
    sessionId: SessionId,
    command: MutationCommand,
    options: MutationOptions = {},
  ): Promise<AdmissionReceipt> {
    const metadata: MutationMetadata = {
      command_id: options.commandId ?? randomUUID(),
      session_id: sessionId,
      run_id: options.runId ?? null,
      expected_session_revision:
        options.expectedSessionRevision === undefined
          ? (this.sessionRevisions.get(sessionId) ?? null)
          : options.expectedSessionRevision,
      trace_id: options.traceId ?? null,
    };
    const body = await this.requestForSession(sessionId, {
      type: "mutate",
      payload: { metadata, command },
    });
    if (body.type !== "admission") {
      throw new HarnessRpcError("expected admission response");
    }
    this.sessionRevisions.set(sessionId, body.payload.session_revision);
    return {
      metadata: body.payload.metadata,
      result: body.payload.result,
      sessionRevision: body.payload.session_revision,
    };
  }

  private requestForSession(
    sessionId: SessionId,
    body: RpcRequestBody,
  ): Promise<RpcResponseBody> {
    return this.request(body, sessionId);
  }

  private requireLifecycleCommands(): void {
    if (!this.capabilities.lifecycle_commands) {
      throw new HarnessRpcError(
        "the connected daemon does not support steer/follow-up lifecycle commands",
      );
    }
  }

  private request(
    body: RpcRequestBody,
    sessionId: SessionId | null = null,
  ): Promise<RpcResponseBody> {
    if (this.closed) {
      return Promise.reject(new HarnessTransportClosedError());
    }
    const id = this.nextRequestId++;
    return new Promise<RpcResponseBody>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new HarnessTimeoutError(
          `request ${id} timed out after ${this.requestTimeoutMs}ms`,
        ));
      }, this.requestTimeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.transport.send({ id, session_id: sessionId, body });
    }).then((responseBody) => {
      if (responseBody.type === "failure") {
        throw HarnessRpcError.fromPayload(responseBody.payload);
      }
      return responseBody;
    });
  }

  private handleResponse(response: RpcResponse): void {
    if (response.id === null) {
      this.handlePush(response.body);
      return;
    }
    const pending = this.pending.get(response.id);
    if (!pending) return;
    this.pending.delete(response.id);
    clearTimeout(pending.timer);
    pending.resolve(response.body);
  }

  private handlePush(body: RpcResponseBody): void {
    if (body.type === "event") {
      const listeners = this.eventListeners.get(body.payload.session_id);
      if (listeners) {
        for (const listener of listeners) listener(body.payload);
      }
      return;
    }
    if (body.type === "event_gap") {
      const gap: EventGap = {
        sessionId: body.payload.session_id,
        lastDeliveredSequence: body.payload.last_delivered_sequence,
        dropped: body.payload.dropped,
        cursorExpired: body.payload.cursor_expired,
      };
      const listeners = this.gapListeners.get(gap.sessionId);
      if (listeners) {
        for (const listener of listeners) listener(gap);
      }
    }
  }

  private handleClose(reason?: Error): void {
    this.closed = true;
    const error = reason ?? new HarnessTransportClosedError();
    for (const [, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }
}

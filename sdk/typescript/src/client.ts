/**
 * High-level client over one connected/managed `harnessd` transport.
 *
 * This mirrors the operations documented in the workspace README's "Talk to
 * a running harnessd" section, using the same `RpcRequestBody`/
 * `RpcResponseBody` contract. It does not invent operations the daemon
 * doesn't support (e.g. `Pause`/`Resume` are wired on the wire but the
 * daemon rejects them today — see `sdk_plan.md` SDK-101).
 */

import { HarnessRpcError, HarnessTransportClosedError, HarnessVersionMismatchError } from "./errors.js";
import { HarnessSession } from "./session.js";
import type { Transport } from "./transport.js";
import {
  AgentEventEnvelope,
  PermissionDecision,
  PermissionId,
  ProtocolCapabilities,
  RpcRequestBody,
  RpcResponse,
  RpcResponseBody,
  SessionId,
  SessionSnapshotWire,
  PROTOCOL_VERSION,
} from "./types.js";

export interface ConnectOptions {
  /** Per-request timeout in milliseconds. Defaults to 30s. */
  requestTimeoutMs?: number;
}

export interface CreateSessionOptions {
  workspaceRoot: string;
  integration: string;
  integrationConfig?: unknown;
  toolset?: string[];
}

type PendingReply = {
  resolve: (body: RpcResponseBody) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

/**
 * Connects to a `harnessd` instance over one {@link Transport} and exposes
 * session lifecycle operations.
 *
 * Construct via {@link HarnessClient.connect}, which performs the mandatory
 * `Hello` handshake before returning.
 */
export class HarnessClient {
  private nextRequestId = 1;
  private readonly pending = new Map<number, PendingReply>();
  private readonly eventListeners = new Map<
    SessionId,
    Set<(event: AgentEventEnvelope) => void>
  >();
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

  /**
   * Perform the mandatory `Hello` handshake over `transport` and return a
   * ready-to-use client. Rejects with {@link HarnessVersionMismatchError} if
   * the daemon reports an incompatible protocol version.
   */
  static async connect(
    transport: Transport,
    options: ConnectOptions = {},
  ): Promise<HarnessClient> {
    const requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    const bootstrap = new HarnessClient(
      transport,
      { resumable_subscribe: false, lifecycle_commands: false },
      requestTimeoutMs,
    );
    const body = await bootstrap.request({
      Hello: { protocol_version: PROTOCOL_VERSION },
    });
    if (typeof body === "string" || !("Hello" in body)) {
      throw new HarnessRpcError(
        "expected a Hello response to the handshake request",
      );
    }
    if (body.Hello.protocol_version !== PROTOCOL_VERSION) {
      throw new HarnessVersionMismatchError(
        PROTOCOL_VERSION,
        body.Hello.protocol_version,
      );
    }
    (bootstrap as { capabilities: ProtocolCapabilities }).capabilities =
      body.Hello.capabilities;
    return bootstrap;
  }

  /** Create a new session and return a {@link HarnessSession} handle. */
  async createSession(options: CreateSessionOptions): Promise<HarnessSession> {
    const body = await this.request({
      CreateSession: {
        workspace_root: options.workspaceRoot,
        integration: options.integration,
        integration_config: options.integrationConfig ?? {},
        toolset: options.toolset ?? [],
      },
    });
    if (typeof body === "string" || !("SessionCreated" in body)) {
      throw new HarnessRpcError("expected SessionCreated response");
    }
    return new HarnessSession(this, body.SessionCreated.session_id);
  }

  /** Send a prompt to start or continue a run on `sessionId`. */
  async prompt(sessionId: SessionId, text: string): Promise<void> {
    await this.requestForSession(sessionId, {
      Prompt: { text, attachments: [] },
    });
  }

  /** Inject input at the active run's next safe command boundary. */
  async steer(sessionId: SessionId, text: string): Promise<void> {
    this.requireLifecycleCommands();
    await this.requestForSession(sessionId, {
      Steer: { text, attachments: [] },
    });
  }

  /** Queue input FIFO to run after the active run completes. */
  async followUp(sessionId: SessionId, text: string): Promise<void> {
    this.requireLifecycleCommands();
    await this.requestForSession(sessionId, {
      FollowUp: { text, attachments: [] },
    });
  }

  /** Cancel the active run on `sessionId`, if any. */
  async cancel(sessionId: SessionId): Promise<void> {
    await this.requestForSession(sessionId, "Cancel");
  }

  /** Resolve a pending permission request on `sessionId`. */
  async resolvePermission(
    sessionId: SessionId,
    id: PermissionId,
    decision: PermissionDecision,
  ): Promise<void> {
    await this.requestForSession(sessionId, {
      ResolvePermission: { id, decision },
    });
  }

  /** Read a point-in-time snapshot of `sessionId`. */
  async snapshot(sessionId: SessionId): Promise<SessionSnapshotWire> {
    const body = await this.requestForSession(sessionId, "Snapshot");
    if (typeof body === "string" || !("Snapshot" in body)) {
      throw new HarnessRpcError("expected Snapshot response");
    }
    return body.Snapshot;
  }

  /**
   * Subscribe to `sessionId`'s event stream, optionally replaying durable
   * events with `session_sequence > sinceSeq` first (see the workspace
   * README's "Durability and resume" section for exactly which event
   * variants are replayable).
   */
  async subscribe(
    sessionId: SessionId,
    onEvent: (event: AgentEventEnvelope) => void,
    sinceSeq: number | null = null,
  ): Promise<() => void> {
    let listeners = this.eventListeners.get(sessionId);
    if (!listeners) {
      listeners = new Set();
      this.eventListeners.set(sessionId, listeners);
    }
    listeners.add(onEvent);

    await this.requestForSession(sessionId, {
      Subscribe: { since_seq: sinceSeq },
    });

    return () => {
      listeners?.delete(onEvent);
    };
  }

  /** Tear the session down on the daemon. */
  async closeSession(sessionId: SessionId): Promise<void> {
    await this.requestForSession(sessionId, "CloseSession");
    this.eventListeners.delete(sessionId);
  }

  /** Close the underlying transport. */
  async close(): Promise<void> {
    await this.transport.close();
  }

  private async requestForSession(
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
        reject(
          new HarnessRpcError(
            `request ${id} timed out after ${this.requestTimeoutMs}ms`,
          ),
        );
      }, this.requestTimeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.transport.send({ id, session_id: sessionId, body });
    }).then((responseBody) => {
      if (typeof responseBody !== "string" && "Error" in responseBody) {
        throw new HarnessRpcError(responseBody.Error.message);
      }
      return responseBody;
    });
  }

  private handleResponse(response: RpcResponse): void {
    if (response.id === null) {
      this.handlePushedEvent(response.body);
      return;
    }
    const pending = this.pending.get(response.id);
    if (!pending) return;
    this.pending.delete(response.id);
    clearTimeout(pending.timer);
    pending.resolve(response.body);
  }

  private handlePushedEvent(body: RpcResponseBody): void {
    if (typeof body === "string" || !("Event" in body)) return;
    const envelope = body.Event;
    const listeners = this.eventListeners.get(envelope.session_id);
    if (!listeners) return;
    for (const listener of listeners) listener(envelope);
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

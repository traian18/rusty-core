/**
 * Ergonomic per-session handle returned by {@link HarnessClient.createSession}.
 */

import type { HarnessClient } from "./client.js";
import type {
  AgentEventEnvelope,
  PermissionDecision,
  PermissionId,
  SessionId,
  SessionSnapshotWire,
} from "./types.js";

export class HarnessSession {
  constructor(
    private readonly client: HarnessClient,
    public readonly sessionId: SessionId,
  ) {}

  /** Send a prompt, starting a new run if the session is idle. */
  prompt(text: string): Promise<void> {
    return this.client.prompt(this.sessionId, text);
  }

  /** Inject input at the active run's next safe command boundary. */
  steer(text: string): Promise<void> {
    return this.client.steer(this.sessionId, text);
  }

  /** Queue input FIFO to run after the active run completes. */
  followUp(text: string): Promise<void> {
    return this.client.followUp(this.sessionId, text);
  }

  /** Cancel the active run, if any. */
  cancel(): Promise<void> {
    return this.client.cancel(this.sessionId);
  }

  /** Resolve a pending tool-call permission request. */
  resolvePermission(
    id: PermissionId,
    decision: PermissionDecision,
  ): Promise<void> {
    return this.client.resolvePermission(this.sessionId, id, decision);
  }

  /** Read a point-in-time snapshot of this session. */
  snapshot(): Promise<SessionSnapshotWire> {
    return this.client.snapshot(this.sessionId);
  }

  /**
   * Subscribe to this session's event stream.
   *
   * Returns an unsubscribe function. Pass `sinceSeq` to replay durable
   * events after reconnecting (see the workspace README's "Durability and
   * resume" section).
   */
  subscribe(
    onEvent: (event: AgentEventEnvelope) => void,
    sinceSeq: number | null = null,
  ): Promise<() => void> {
    return this.client.subscribe(this.sessionId, onEvent, sinceSeq);
  }

  /**
   * Async-iterable event stream, for `for await (const event of session.events())`
   * usage instead of a callback.
   */
  async *events(
    sinceSeq: number | null = null,
  ): AsyncGenerator<AgentEventEnvelope, void, void> {
    const queue: AgentEventEnvelope[] = [];
    let wake: (() => void) | null = null;
    let done = false;

    const unsubscribe = await this.subscribe((event) => {
      queue.push(event);
      wake?.();
    }, sinceSeq);

    try {
      while (!done) {
        if (queue.length === 0) {
          await new Promise<void>((resolve) => {
            wake = resolve;
          });
          wake = null;
        }
        while (queue.length > 0) {
          yield queue.shift() as AgentEventEnvelope;
        }
      }
    } finally {
      unsubscribe();
      done = true;
    }
  }

  /** Tear this session down on the daemon. */
  close(): Promise<void> {
    return this.client.closeSession(this.sessionId);
  }
}

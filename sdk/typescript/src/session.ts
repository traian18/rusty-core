/** Ergonomic per-session handle returned by HarnessClient. */

import type {
  AdmissionReceipt,
  EventGap,
  HarnessClient,
  MutationOptions,
} from "./client.js";
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

  prompt(text: string, options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.client.prompt(this.sessionId, text, options);
  }

  steer(text: string, options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.client.steer(this.sessionId, text, options);
  }

  followUp(text: string, options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.client.followUp(this.sessionId, text, options);
  }

  cancel(options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.client.cancel(this.sessionId, options);
  }

  resolvePermission(
    id: PermissionId,
    decision: PermissionDecision,
    options?: MutationOptions,
  ): Promise<AdmissionReceipt> {
    return this.client.resolvePermission(this.sessionId, id, decision, options);
  }

  snapshot(): Promise<SessionSnapshotWire> {
    return this.client.snapshot(this.sessionId);
  }

  subscribe(
    onEvent: (event: AgentEventEnvelope) => void,
    sinceSeq: number | null = null,
    onGap?: (gap: EventGap) => void,
  ): Promise<() => void> {
    return this.client.subscribe(this.sessionId, onEvent, sinceSeq, onGap);
  }

  async *events(
    sinceSeq: number | null = null,
    onGap?: (gap: EventGap) => void,
  ): AsyncGenerator<AgentEventEnvelope, void, void> {
    const queue: AgentEventEnvelope[] = [];
    let wake: (() => void) | null = null;
    let done = false;
    const unsubscribe = await this.subscribe((event) => {
      queue.push(event);
      wake?.();
    }, sinceSeq, onGap);

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
      done = true;
      unsubscribe();
    }
  }

  close(options?: MutationOptions): Promise<AdmissionReceipt> {
    return this.client.closeSession(this.sessionId, options);
  }
}

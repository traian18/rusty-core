/** Typed errors for protocol-v2 clients. */

import type { RpcErrorCategory, RpcErrorPayload } from "./types.js";

export class HarnessSdkError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = new.target.name;
  }
}

export class HarnessRpcError extends HarnessSdkError {
  constructor(
    message: string,
    public readonly code = "rpc.legacy_error",
    public readonly category: RpcErrorCategory = "protocol",
    public readonly retryable = false,
    public readonly details?: unknown,
    public readonly traceId?: string,
    public readonly runId?: string,
    public readonly providerRequestId?: string,
  ) {
    super(message);
  }

  static fromPayload(payload: RpcErrorPayload): HarnessRpcError {
    return new HarnessRpcError(
      payload.message,
      payload.code,
      payload.category,
      payload.retryable,
      payload.details,
      payload.trace_id,
      payload.run_id,
      payload.provider_request_id,
    );
  }
}

export class HarnessTransportClosedError extends HarnessSdkError {
  constructor(message = "transport closed before a response was received") {
    super(message);
  }
}

export class HarnessTimeoutError extends HarnessSdkError {
  constructor(message = "request timed out") {
    super(message);
  }
}

export class HarnessVersionMismatchError extends HarnessSdkError {
  constructor(
    public readonly expected: number,
    public readonly actual: number,
  ) {
    super(
      `protocol version mismatch: sdk expects ${expected}, daemon reported ${actual}`,
    );
  }
}

export class HarnessProtocolError extends HarnessSdkError {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
  }
}

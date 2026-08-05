/**
 * Typed errors for the TypeScript SDK.
 *
 * The current wire protocol only carries an untyped `{ message: string }`
 * on `RpcResponseBody::Error` (see `sdk_plan.md` SDK-200, "typed errors"
 * gap). `HarnessRpcError.message` is that raw server string until the
 * protocol grows a structured error code/category/details payload.
 */

/** Base class for every error this SDK throws. */
export class HarnessSdkError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = new.target.name;
  }
}

/** The daemon returned `RpcResponseBody::Error` for a request. */
export class HarnessRpcError extends HarnessSdkError {
  constructor(message: string) {
    super(message);
  }
}

/** The transport disconnected (process exit, socket close) before a reply. */
export class HarnessTransportClosedError extends HarnessSdkError {
  constructor(message = "transport closed before a response was received") {
    super(message);
  }
}

/** A request did not receive a response within its timeout. */
export class HarnessTimeoutError extends HarnessSdkError {
  constructor(message = "request timed out") {
    super(message);
  }
}

/**
 * The daemon's `Hello` response reported a `protocol_version` this SDK does
 * not support.
 */
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

/** A frame from the daemon could not be parsed as JSON or did not match the expected envelope shape. */
export class HarnessProtocolError extends HarnessSdkError {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
  }
}

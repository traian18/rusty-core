/**
 * Transport abstraction plus a managed stdio sidecar transport.
 *
 * Additional transports (WebSocket, Unix/Windows IPC) should implement the
 * same {@link Transport} interface; see `sdk_plan.md` SDK-301/SDK-303.
 */

import { ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import { createInterface } from "node:readline";

import { HarnessTransportClosedError } from "./errors.js";
import type { RpcRequest, RpcResponse } from "./types.js";

export interface Transport {
  /** Send one request frame. Does not wait for a reply. */
  send(request: RpcRequest): void;
  /** Register a listener invoked for every decoded response/event frame. */
  onMessage(listener: (response: RpcResponse) => void): void;
  /** Register a listener invoked once when the transport closes. */
  onClose(listener: (reason?: Error) => void): void;
  /** Close the transport and release any underlying resources. */
  close(): Promise<void>;
}

export interface StdioSidecarOptions {
  /** Path to the `harnessd` binary. */
  command: string;
  /** Extra arguments; `--stdio` is added automatically if absent. */
  args?: string[];
  /** Working directory for the spawned process. */
  cwd?: string;
  /** Environment variables for the spawned process (merged over `process.env` is the caller's choice, not implied here). */
  env?: NodeJS.ProcessEnv;
  /** Called with each raw stderr line, e.g. for structured log forwarding. */
  onStderrLine?: (line: string) => void;
}

/**
 * Spawns `harnessd --stdio` as a child process and frames requests/responses
 * as newline-delimited JSON, matching `crates/transports/stdio`.
 *
 * Per the workspace README: the daemon never writes protocol frames to
 * stdout, only structured logs to stderr, so stdout framing is safe to
 * parse line-by-line without a length prefix.
 *
 * This is a managed-sidecar transport (`sdk_plan.md` §3): the SDK owns the
 * child process lifecycle. It does not yet implement restart/backoff,
 * version-mismatch handling beyond surfacing the daemon's `Hello` reply, or
 * process-tree termination guarantees on Windows — see SDK-303.
 */
export class StdioSidecarTransport implements Transport {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly messageListeners: Array<(response: RpcResponse) => void> =
    [];
  private readonly closeListeners: Array<(reason?: Error) => void> = [];
  private closed = false;

  constructor(options: StdioSidecarOptions) {
    const args = options.args ?? [];
    const finalArgs = args.includes("--stdio") ? args : [...args, "--stdio"];

    this.child = spawn(options.command, finalArgs, {
      cwd: options.cwd,
      env: options.env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    const stdoutLines = createInterface({ input: this.child.stdout });
    stdoutLines.on("line", (line) => this.handleLine(line));

    const stderrLines = createInterface({ input: this.child.stderr });
    stderrLines.on("line", (line) => options.onStderrLine?.(line));

    this.child.once("exit", (code, signal) => {
      this.closed = true;
      const reason =
        code === 0 || code === null
          ? undefined
          : new Error(
              `harnessd exited with code ${code ?? "null"} (signal ${signal ?? "none"})`,
            );
      for (const listener of this.closeListeners) listener(reason);
    });

    this.child.once("error", (error) => {
      this.closed = true;
      for (const listener of this.closeListeners) listener(error);
    });
  }

  private handleLine(line: string): void {
    const trimmed = line.trim();
    if (trimmed.length === 0) return;
    let parsed: RpcResponse;
    try {
      parsed = JSON.parse(trimmed) as RpcResponse;
    } catch (cause) {
      // A malformed frame should not crash the process; surface it as a
      // close reason so callers notice instead of silently dropping data.
      for (const listener of this.closeListeners) {
        listener(
          new Error(`received malformed frame from harnessd: ${trimmed}`, {
            cause,
          }),
        );
      }
      return;
    }
    for (const listener of this.messageListeners) listener(parsed);
  }

  send(request: RpcRequest): void {
    if (this.closed) {
      throw new HarnessTransportClosedError();
    }
    this.child.stdin.write(JSON.stringify(request) + "\n");
  }

  onMessage(listener: (response: RpcResponse) => void): void {
    this.messageListeners.push(listener);
  }

  onClose(listener: (reason?: Error) => void): void {
    this.closeListeners.push(listener);
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.child.stdin.end();
    this.child.kill();
  }
}
